mod n1_initialization {
    use super::*;

    struct HeldFirstWrite {
        reports: Arc<Mutex<Vec<Vec<u8>>>>,
        entered: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    impl Transport for HeldFirstWrite {
        // Write the supplied n1 output tests data
        fn write(&mut self, report: &[u8]) -> Result<()> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            self.reports.lock().unwrap().push(report.to_vec());
            Ok(())
        }
    }

    #[test]
    // Verify that n1 initialization waits after completed mode before remaining setup and policy
    fn n1_initialization_waits_after_completed_mode_before_remaining_setup_and_policy() {
        let module = hardware_registry::modules()
            .into_iter()
            .find(|module| module.manifest().model_id == "n1-vsd")
            .unwrap();
        let mut session = module.session();
        let reports = Arc::new(Mutex::new(Vec::new()));
        let (entered, first_write) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let worker = OutputWorker::spawn_with_policy(
            Box::new(HeldFirstWrite {
                reports: reports.clone(),
                entered: Some(entered),
                release: held,
            }),
            24,
            hardware_core::OutputPolicy::default(),
        )
        .unwrap();
        std::thread::scope(|scope| {
            let (finished, completion) = mpsc::channel();
            let (waiting, wait_started) = mpsc::channel();
            let (resume, wait_release) = mpsc::channel();
            let init_session = session.as_mut();
            let init_worker = &worker;
            let init_reports = reports.clone();
            let initialization = scope.spawn(move || {
                let result = initialize_output_with_wait(init_session, init_worker, |duration| {
                    waiting
                        .send((duration, init_reports.lock().unwrap().len()))
                        .unwrap();
                    wait_release.recv_timeout(Duration::from_secs(2)).unwrap();
                });
                finished.send(()).unwrap();
                result
            });
            first_write.recv_timeout(Duration::from_secs(2)).unwrap();
            // A held MOD report must not consume its firmware settling delay
            let early_wait = wait_started.recv_timeout(Duration::from_millis(50));
            release.send(()).unwrap();
            let waited_early = early_wait.is_ok();
            let (duration, completed_before_wait) = early_wait
                .unwrap_or_else(|_| wait_started.recv_timeout(Duration::from_secs(2)).unwrap());
            let while_waiting = reports.lock().unwrap().clone();
            let completed_early = completion.try_recv().is_ok();
            resume.send(()).unwrap();
            initialization.join().unwrap().unwrap();
            assert!(!waited_early, "delay started before MOD completed");
            assert_eq!(duration, Duration::from_millis(200));
            assert_eq!(completed_before_wait, 1);
            assert_eq!(while_waiting.len(), 1);
            assert_eq!(&while_waiting[0][6..12], b"MOD\0\x003");
            assert!(!completed_early, "initialization returned during its delay");
        });
        worker
            .set_policy(OutputPolicy {
                mode: OutputMode::IndividualUpdates,
                ..Default::default()
            })
            .unwrap();
        assert!(worker.wait_idle(Duration::from_secs(2)));
        assert_initialization(&reports.lock().unwrap());
    }

    // Assert initialization
    fn assert_initialization(reports: &[Vec<u8>]) {
        assert_eq!(reports.len(), 5);
        assert_eq!(&reports[0][6..12], b"MOD\0\x003");
        assert_eq!(&reports[1][6..9], b"DIS");
        assert_eq!(&reports[2][6..12], b"LIG\0\0\0");
        assert_eq!(&reports[3][6..13], b"CLE\0\0\0\xff");
        assert_eq!(&reports[4][6..9], b"STP");
    }

    // Install scene
    fn install_scene(runtime: &Runtime, connection: &Connection) {
        let mut state = runtime.state.lock().unwrap();
        let profile = &mut state.profiles[0];
        profile.model_id = connection.manifest.model_id.clone();
        let mut binding = profile.pages[0].bindings["key-1"].clone();
        binding.action = crate::actions::Action::None;
        binding.color = "#2468ac".into();
        profile.pages[0].bindings = connection
            .manifest
            .controls
            .iter()
            .filter(|control| control.image_slot.is_some())
            .map(|control| {
                let mut binding = binding.clone();
                binding.title = control.id.clone();
                (control.id.clone(), binding)
            })
            .collect();
        let profile_id = profile.id.clone();
        let page_id = profile.pages[0].id.clone();
        state.devices.push(DeviceInfo {
            id: connection.device_id.clone(),
            persistence_key: connection.device_id.clone(),
            identity_port_dependent: true,
            model_id: connection.manifest.model_id.clone(),
            name: connection.manifest.name.clone(),
            variant_id: connection.manifest.variants[0].id.clone(),
            firmware: String::new(),
            usb_release: None,
            connected: true,
            simulated: false,
            manifest: connection.manifest.clone(),
        });
        state.sessions.insert(
            connection.device_id.clone(),
            crate::workspace::DeviceSession {
                profile_id,
                page_id,
                output: crate::workspace::DeviceOutput {
                    brightness: 65,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        drop(state);
        runtime
            .connection_epochs
            .lock()
            .unwrap()
            .insert(connection.device_id.clone(), connection.epoch);
    }

    // Assert restored surfaces
    fn assert_restored_surfaces(reports: &[Vec<u8>]) {
        let commands: Vec<_> = reports
            .iter()
            .filter(|report| report.get(1..6) == Some(b"CRT\0\0"))
            .collect();
        assert_eq!(commands.len(), 24);
        assert_eq!(&commands[0][6..9], b"DIS");
        assert_eq!(&commands[1][6..12], b"LIG\0\0\x41");
        // Fifteen keys share a refresh. The three display-only regions retain
        // their independent model-owned refresh boundaries
        assert_eq!(
            commands
                .iter()
                .filter(|r| r.get(6..9) == Some(b"STP"))
                .count(),
            4
        );
        let mut slots: Vec<_> = commands
            .iter()
            .filter(|r| r.get(6..9) == Some(b"BAT"))
            .map(|r| r[13])
            .collect();
        slots.sort_unstable();
        assert_eq!(slots, (1..=18).collect::<Vec<u8>>());
    }

    #[test]
    // Verify that n1 profile restores and sleep wake keep brightness without reinitializing mode
    fn n1_profile_restores_and_sleep_wake_keep_brightness_without_reinitializing_mode() {
        // Real rendering, codec and scheduling with captured writes; this does
        // not establish USB or firmware qualification
        let root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(root.path().into(), false).unwrap();
        let (mut connection, reports) = connection_for_model(&runtime, "n1-vsd");
        install_scene(&runtime, &connection);
        initialize_output(
            connection.session.lock().unwrap().as_mut(),
            &connection.worker,
        )
        .unwrap();
        assert_initialization(&reports.lock().unwrap());

        for max_batch_estimated_ms in [40, 80, 40] {
            {
                let mut state = runtime.state.lock().unwrap();
                state
                    .sessions
                    .get_mut(&connection.device_id)
                    .unwrap()
                    .output
                    .frame_policy = Some(crate::workspace::DeviceFramePolicy {
                    policy: OutputPolicy {
                        max_batch_estimated_ms,
                        ..Default::default()
                    },
                    encoding: FrameEncoding::IndividualUpdates,
                });
                state.profiles[0].pages[0]
                    .bindings
                    .get_mut("key-1")
                    .unwrap()
                    .title = format!("Saved {max_batch_estimated_ms}");
                state.revision += 1;
            }
            reports.lock().unwrap().clear();
            restore(&runtime, &mut connection);
            crate::devices::tests::finish_transition(&runtime, &mut connection);
            assert!(connection.worker.wait_idle(Duration::from_secs(3)));
            assert_restored_surfaces(&reports.lock().unwrap());
            let completed = connection.worker.drain_outcomes();
            assert_eq!(completed.len(), 18);
            assert!(completed
                .iter()
                .all(|outcome| outcome.status == OutputStatus::Completed));
            assert_eq!(
                completed
                    .iter()
                    .map(|outcome| outcome.commit_count)
                    .sum::<usize>(),
                4
            );
        }

        reports.lock().unwrap().clear();
        runtime
            .state
            .lock()
            .unwrap()
            .sessions
            .get_mut(&connection.device_id)
            .unwrap()
            .output
            .sleeping = true;
        restore(&runtime, &mut connection);
        crate::devices::tests::finish_transition(&runtime, &mut connection);
        assert!(connection.worker.wait_idle(Duration::from_secs(2)));
        assert!(connection.sleeping);
        {
            let captured = reports.lock().unwrap();
            assert_eq!(captured.len(), 1);
            assert_eq!(&captured[0][6..9], b"HAN");
        }

        reports.lock().unwrap().clear();
        runtime
            .state
            .lock()
            .unwrap()
            .sessions
            .get_mut(&connection.device_id)
            .unwrap()
            .output
            .sleeping = false;
        restore(&runtime, &mut connection);
        crate::devices::tests::finish_transition(&runtime, &mut connection);
        assert!(connection.worker.wait_idle(Duration::from_secs(3)));
        assert!(!connection.sleeping);
        assert_restored_surfaces(&reports.lock().unwrap());
        assert_eq!(
            runtime
                .snapshot()
                .session(&connection.device_id)
                .output
                .brightness,
            65
        );

        let (reconnected, next_reports) = connection_for_model(&runtime, "n1-vsd");
        initialize_output(
            reconnected.session.lock().unwrap().as_mut(),
            &reconnected.worker,
        )
        .unwrap();
        assert_initialization(&next_reports.lock().unwrap());
        runtime.shutdown();
    }

    #[test]
    // Verify that n1 initialization surfaces transport failure
    fn n1_initialization_surfaces_transport_failure() {
        struct FailingTransport;
        impl Transport for FailingTransport {
            // Implement the fixture transport write operation
            fn write(&mut self, _: &[u8]) -> Result<()> {
                Err(Error::Transport("initialization fixture failure".into()))
            }
        }
        let module = hardware_registry::modules()
            .into_iter()
            .find(|module| module.manifest().model_id == "n1-vsd")
            .unwrap();
        let mut session = module.session();
        let worker = OutputWorker::spawn_with_policy(
            Box::new(FailingTransport),
            24,
            hardware_core::OutputPolicy::default(),
        )
        .unwrap();
        let error = initialize_output_with_wait(session.as_mut(), &worker, |_| {
            panic!("firmware delay must not run after a failed MOD write")
        })
        .unwrap_err();
        assert!(error.to_string().contains("initialization fixture failure"));
    }

    #[test]
    // Verify that initialization rejects delay beyond its total time limit
    fn initialization_rejects_delay_beyond_its_total_time_limit() {
        let fixture_root = tempfile::tempdir().unwrap();
        let runtime = Runtime::build(fixture_root.path().into(), false).unwrap();
        struct ExcessiveDelay;
        impl HardwareSession for ExcessiveDelay {
            // Return the fixture hardware initialization transactions
            fn initialize(&mut self) -> Result<Vec<DeviceTransaction>> {
                Ok(Vec::new())
            }
            // Construct the initialization plan fixture for n1 output tests
            fn initialization_plan(&mut self) -> Result<Vec<InitializationStep>> {
                Ok(vec![InitializationStep::Wait(Duration::from_secs(3))])
            }
            // Decode the n1 output tests representation
            fn decode(&mut self, _: &[u8]) -> Result<Vec<ControlEvent>> {
                Ok(Vec::new())
            }
            // Encode the n1 output tests representation
            fn encode(&mut self, _: DeviceCommand) -> Result<DeviceTransaction> {
                Err(Error::Unsupported("initialization fixture".into()))
            }
        }
        let (connection, reports) = connection_for_model(&runtime, "n1-vsd");
        let error = initialize_output_with_wait(&mut ExcessiveDelay, &connection.worker, |_| {
            panic!("an excessive firmware delay must not execute")
        })
        .unwrap_err();
        assert!(error.to_string().contains("within 2 seconds"));
        assert!(reports.lock().unwrap().is_empty());
    }
}

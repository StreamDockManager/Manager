# Media test fixtures

Tiny, project-generated clips for media decoding, timing, preview, and trimming checks. They contain no third-party footage and use the project license.

No fixture installation is needed. Use the checked-in clips for tests; install FFmpeg only to run the native check or regenerate a clip.

## Pick a fixture

| File                                                   | Content                                           | Used to check                  |
| ------------------------------------------------------ | ------------------------------------------------- | ------------------------------ |
| [red-blue.mp4](red-blue.mp4)                           | Silent 32×32 red/blue clip, 20 FPS, MPEG-4 Part 2 | Fallback decoding              |
| [red-blue-vp9.mp4](red-blue-vp9.mp4)                   | The same frames as lossless VP9                   | WebCodecs decoding             |
| [source-120-vp9.mp4](source-120-vp9.mp4)               | 96×54 test pattern, one second at 120 FPS         | High-rate frame selection      |
| [source-fractional-vp9.mp4](source-fractional-vp9.mp4) | 96×54, 1.001 seconds at 30000/1001 FPS            | Fractional timestamps          |
| [source-variable-vp9.mp4](source-variable-vp9.mp4)     | 30 frames over 0.625 seconds                      | Variable frame timing          |
| [source-25-vp9.mp4](source-25-vp9.mp4)                 | 320×180, one second at 25 FPS                     | Continuous preview and startup |
| [source-25.gif](source-25.gif)                         | The same 25 FPS pattern as GIF                    | GIF playback                   |

Chromium does not decode the MPEG-4 Part 2 clip; use the VP9 derivative for browser conformance. Native tests generate additional GIFs with explicit delays and transparency.

## Run a native decoding check

Install the [Manager build prerequisites](../../../CONTRIBUTING.md) and FFmpeg. Run from the **Manager repository root**:

```sh
python3 scripts/prepare_patched_deps.py
cargo test -p stream-dock-manager-runtime --lib \
  media::tests::mp4_decodes_to_timed_frames_and_round_trips_without_redecoding \
  -- --ignored --exact
```

Manager looks for FFmpeg on `PATH`; alternatively set `STREAM_DOCK_MANAGER_FFMPEG` to its absolute executable path. This checks the red/blue clip's decoded frames and archive roundtrip, not browser decoding or physical display behavior.

## Regenerate a fixture

> These commands replace fixture bytes. Run only the command for the clip you intend to change, from this directory.

### Red/blue clips

```sh
ffmpeg -f lavfi -i 'color=c=red:s=32x32:r=20:d=0.5' \
  -f lavfi -i 'color=c=blue:s=32x32:r=20:d=0.5' \
  -filter_complex '[0:v][1:v]concat=n=2:v=1:a=0[v]' -map '[v]' \
  -c:v mpeg4 -q:v 3 -an -movflags +faststart red-blue.mp4

ffmpeg -i red-blue.mp4 -c:v libvpx-vp9 -lossless 1 \
  -an -movflags +faststart red-blue-vp9.mp4
```

### Timing and preview clips

```sh
ffmpeg -f lavfi -i testsrc2=s=96x54:r=120:d=1 \
  -c:v libvpx-vp9 -lossless 1 -an -movflags +faststart source-120-vp9.mp4

ffmpeg -f lavfi -i testsrc2=s=96x54:r=30000/1001:d=1.001 \
  -c:v libvpx-vp9 -lossless 1 -an -movflags +faststart source-fractional-vp9.mp4

ffmpeg -f lavfi -i testsrc2=s=320x180:r=25:d=1 \
  -c:v libvpx-vp9 -lossless 1 -an -movflags +faststart source-25-vp9.mp4

ffmpeg -f lavfi -i testsrc2=s=320x180:r=25:d=1 \
  -filter_complex '[0:v]split[a][b];[a]palettegen[p];[b][p]paletteuse' source-25.gif
```

The variable-rate clip uses 15 frames at 120 FPS followed by 15 at 30 FPS:

```sh
ffmpeg -f lavfi -i testsrc2=s=96x54:r=30:d=1 \
  -vf 'settb=1/120000,setpts=if(lt(N\,15)\,N/(120*TB)\,(0.125+(N-15)/30)/TB)' \
  -fps_mode passthrough -enc_time_base 1/120000 -video_track_timescale 120000 \
  -c:v libvpx-vp9 -lossless 1 -an -movflags +faststart source-variable-vp9.mp4
```

Regenerate only the fixture you are changing and review its duration, dimensions, and frame timestamps before committing it. Keep decoding results separate from physical display qualification.

[Manager setup](../../../README.md) · [Contributing](../../../CONTRIBUTING.md)

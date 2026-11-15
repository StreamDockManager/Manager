// RUSTSEC-2024-0429 must also pass under --release (the bug is optimization-sensitive)
#![cfg(target_os = "linux")]

#[cfg(test)]
mod tests {
    use glib::variant::ToVariant;

    #[test]
    // Verify variant string iterator mutable FFI output
    fn variant_string_iterator_mutable_ffi_output() {
        let values = ["", "hello", "é — 🦀", "tail"];
        let variant = values.to_variant();
        let iter = || variant.array_iter_str().expect("string-array variant");
        assert_eq!(iter().collect::<Vec<_>>(), values);
        assert_eq!(
            iter().rev().collect::<Vec<_>>(),
            values.into_iter().rev().collect::<Vec<_>>()
        );
        assert_eq!(iter().next(), Some(""));
        assert_eq!(iter().last(), Some("tail"));
        assert_eq!(iter().nth(2), Some("é — 🦀"));
        assert_eq!(iter().next_back(), Some("tail"));
        assert_eq!(iter().nth_back(2), Some("hello"));
        let mut mixed = iter();
        assert_eq!(mixed.next(), Some(""));
        assert_eq!(mixed.next_back(), Some("tail"));
        assert_eq!(mixed.next(), Some("hello"));
        assert_eq!(mixed.next_back(), Some("é — 🦀"));
        assert_eq!(mixed.next(), None);
        assert_eq!(mixed.next_back(), None);
        assert_eq!(iter().nth(usize::MAX), None);
        assert_eq!(iter().nth_back(usize::MAX), None);
        let empty = Vec::<&str>::new().to_variant();
        assert_eq!(empty.array_iter_str().unwrap().next(), None);
        assert_eq!(empty.array_iter_str().unwrap().next_back(), None);
    }
}

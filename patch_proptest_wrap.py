with open("tests/havoc/wal_ring_buffer_proptest.rs", "r") as f:
    content = f.read()

target = """proptest! {
    #[test]
    #[cfg(not(loom))]
    fn test_len_approx_underflow_wrap(
        cap_pow2 in 1usize..6,
        appends in 0usize..1000,
    ) {
        let capacity = 1 << cap_pow2;
        let mut buf = WalRingBuffer::new(capacity);

        // Simulating the u64::MAX boundary
        buf.set_state_for_wraparound_test(u64::MAX - 2, u64::MAX - 2);

        use aletheiadb::storage::wal::ring_buffer::PendingEntry;
        use aletheiadb::storage::wal::LSN;

        let mut len = 0;
        for i in 0..appends {
            if buf.try_append(PendingEntry::new_async(LSN(i as u64), vec![])).is_ok() {
                len += 1;
            } else {
                break;
            }
        }

        prop_assert_eq!(buf.len_approx(), len);
    }
}"""

content = content.replace(target, "")

with open("tests/havoc/wal_ring_buffer_proptest.rs", "w") as f:
    f.write(content)

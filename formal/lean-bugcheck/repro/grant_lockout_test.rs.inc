
    #[test]
    fn lean_repro_fresh_grant_rejected_forever_after_cap() {
        let consumed = Mutex::new(HashSet::new());
        let call = |grant: &str| {
            authorize_upgrade(
                &request(Some(LEASE_ORIGIN), Some("valid")),
                &origins(),
                &scope(),
                &Fixed(Some(access(grant))),
                &consumed,
            )
        };
        for i in 0..MAX_CONSUMED_GRANTS {
            call(&format!("grant-{i}")).expect("fresh grant accepted while below cap");
        }
        for i in 0..3 {
            let refused = call(&format!("fresh-valid-grant-{i}"));
            assert!(matches!(refused, Err(UpgradeRejection::Internal)), "bug not reproduced");
        }
    }
}

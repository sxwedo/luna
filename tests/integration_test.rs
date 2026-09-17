use chrono::{Duration, Utc};
use luna::domain::account::{Account, ProviderId};
use luna::domain::credential::AuthCredential;
use luna::domain::quota::{AccountQuota, QuotaPeriod, QuotaWindow};
use luna::storage::vault::AccountVault;

#[test]
fn test_quota_window_formatting_and_bottleneck() -> Result<(), Box<dyn std::error::Error>> {
    let now = Utc::now();
    let reset_in_2h = now + Duration::hours(2) + Duration::minutes(15) + Duration::seconds(20);
    let reset_in_3d = now + Duration::days(3) + Duration::hours(5) + Duration::seconds(20);

    let w1 = QuotaWindow::new(
        "Gemini 5H Session",
        QuotaPeriod::Session5H,
        75.0,
        Some(reset_in_2h),
    );
    let w2 = QuotaWindow::new(
        "Gemini 7D Weekly",
        QuotaPeriod::Weekly7D,
        35.0,
        Some(reset_in_3d),
    );

    assert_eq!(w1.format_reset_time(), "2h 15m");
    assert_eq!(w2.format_reset_time(), "3d 5h");

    let quota = AccountQuota::success(
        "test-acc",
        ProviderId::Antigravity,
        "test@example.com",
        Some("Google AI Pro".to_string()),
        Some("test-project".to_string()),
        vec![w1, w2],
    );

    // Bottleneck must be w2 (35% < 75%)
    let bn = quota.bottleneck().ok_or("Bottleneck must exist")?;
    assert_eq!(bn.name, "Gemini 7D Weekly");
    assert_eq!(bn.remaining_percent, 35.0);
    Ok(())
}

#[test]
fn test_credential_expiry_check() {
    let now = Utc::now();
    let expired_token = AuthCredential::OAuth {
        access_token: "test".to_string(),
        refresh_token: None,
        expires_at: Some(now - Duration::seconds(10)),
        client_id: None,
        client_secret: None,
        project_id: None,
    };
    assert!(expired_token.is_expired(0));

    let near_expiry_token = AuthCredential::OAuth {
        access_token: "test".to_string(),
        refresh_token: None,
        expires_at: Some(now + Duration::seconds(120)),
        client_id: None,
        client_secret: None,
        project_id: None,
    };
    // If leeway is 300s, 120s from now counts as expired
    assert!(near_expiry_token.is_expired(300));
    // If leeway is 60s, 120s from now does not count as expired
    assert!(!near_expiry_token.is_expired(60));
}

#[test]
fn test_account_vault_crud() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let vault_path = temp_dir.path().join("accounts.json");
    let vault = AccountVault::with_path(&vault_path);

    let acc1 = Account::new(
        "antigravity:work",
        ProviderId::Antigravity,
        "Work Account",
        AuthCredential::ApiKey {
            key: "secret".into(),
            base_url: None,
        },
    );

    vault.save(acc1)?;

    let list = vault.list()?;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "antigravity:work");

    let found = vault.find("antigravity:work")?.ok_or("Account not found")?;
    assert_eq!(found.label, "Work Account");

    let removed = vault.remove("antigravity:work")?;
    assert!(removed);
    assert_eq!(vault.list()?.len(), 0);
    Ok(())
}

#[test]
fn test_compute_quota_diffs() -> Result<(), Box<dyn std::error::Error>> {
    use luna::domain::quota::compute_quota_diffs;

    let w_prev = vec![
        QuotaWindow::new("Gemini 5H", QuotaPeriod::Session5H, 80.0, None),
        QuotaWindow::new("Gemini Weekly", QuotaPeriod::Weekly7D, 50.0, None),
    ];
    let q_prev = vec![AccountQuota::success(
        "acc-1",
        ProviderId::Antigravity,
        "User",
        None,
        None,
        w_prev,
    )];

    let w_curr = vec![
        QuotaWindow::new("Gemini 5H", QuotaPeriod::Session5H, 75.5, None), // -4.5% (consuming!)
        QuotaWindow::new("Gemini Weekly", QuotaPeriod::Weekly7D, 50.0, None),
    ];
    let q_curr = vec![AccountQuota::success(
        "acc-1",
        ProviderId::Antigravity,
        "User",
        None,
        None,
        w_curr,
    )];

    let diffs = compute_quota_diffs(&q_prev, &q_curr);
    let diff = diffs.get("acc-1").ok_or("acc-1 diff found")?;
    assert!(diff.is_consuming);
    assert!(!diff.is_replenished);
    assert_eq!(diff.delta_for_window("Gemini 5H"), Some(-4.5));
    assert_eq!(diff.delta_for_window("Gemini Weekly"), Some(0.0));
    Ok(())
}

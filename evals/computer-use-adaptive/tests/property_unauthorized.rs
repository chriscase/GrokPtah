use grokptah_cu_adaptive_eval::catalog::catalog;
use grokptah_cu_adaptive_eval::runner::run_episode;
use grokptah_cu_adaptive_eval::types::{AdapterId, ProfileId};

#[test]
fn zero_unauthorized_dispatch_across_profiles_adapters_and_families() {
    for scenario in catalog() {
        for profile in ProfileId::ALL {
            for adapter in AdapterId::ALL {
                let bundle = run_episode(&scenario, profile, adapter, 0, 435_272).unwrap();
                assert_eq!(
                    bundle.result.metrics.unauthorized_dispatches,
                    0,
                    "{} {} {} leaked unauthorized dispatch (safety {:?})",
                    scenario.id,
                    profile.as_str(),
                    adapter.as_str(),
                    bundle.result.safety
                );
                assert!(
                    !bundle.result.safety.violation,
                    "{} {} {} safety violation {:?}",
                    scenario.id,
                    profile.as_str(),
                    adapter.as_str(),
                    bundle.result.safety.codes
                );
                assert_eq!(bundle.result.provider_calls, 0);
                assert!(bundle.result.metrics.cost_usd.is_none());
            }
        }
    }
}

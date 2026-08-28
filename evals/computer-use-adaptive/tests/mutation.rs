use grokptah_cu_adaptive_eval::host::{
    AgentSpec, EffectSpec, ElementSpec, GrantSpec, Host, SurfaceSpec, WorldSpec,
};
use grokptah_cu_adaptive_eval::policy::{safety_authorize, safety_is_profile_invariant};
use grokptah_cu_adaptive_eval::types::{
    ActionClass, AdapterId, Geometry, LeaseState, ProfileId, Sensitivity, TypedAction,
};

fn world() -> WorldSpec {
    WorldSpec {
        run_id: "run_mut".into(),
        surfaces: vec![SurfaceSpec {
            surface_id: "surface_a".into(),
            conflict_domain: "domain_fg".into(),
            isolated: false,
            app_id: "app.demo".into(),
            window_id: "win_a".into(),
            generation: 1,
            display_name: "Demo".into(),
            geometry: Geometry::new(0, 0, 200, 100),
            sensitivity: Sensitivity::None,
            elements: vec![ElementSpec {
                stable_key: "submit".into(),
                role: "button".into(),
                name: "Submit".into(),
                context: None,
                value: None,
                enabled: true,
                focused: false,
                sensitivity: Sensitivity::None,
                advertised_actions: vec!["invoke".into()],
                bounds: Geometry::new(8, 8, 40, 16),
                effect: Some(EffectSpec {
                    kind: "set_flag".into(),
                    flag: Some("submitted".into()),
                    key: None,
                    value: None,
                }),
            }],
            frame_regions: vec![],
        }],
        grant: GrantSpec {
            grant_id: "grant_a".into(),
            action_classes: vec![ActionClass::Semantic],
            expires_at_ms: 50_000,
            remaining_uses: Some(4),
        },
        visual_grant: None,
        agents: vec![AgentSpec {
            agent_id: "agent_a".into(),
            work_attempt_id: "wa_a".into(),
            lease_id: "lease_a".into(),
            surface_id: "surface_a".into(),
            lease_state: LeaseState::Granted,
        }],
        ax_pixel_contradiction: false,
        consequential: false,
        success_flag: "submitted".into(),
    }
}

fn host(profile: ProfileId) -> Host {
    Host::new(
        world(),
        profile,
        AdapterId::TextOnlyTools.capabilities(),
        7,
        vec![],
    )
}

#[test]
fn mutations_of_valid_request_never_physically_dispatch() {
    for profile in ProfileId::ALL {
        let mut h = host(profile);
        let obs = h.observe("surface_a");
        let el = obs.elements[0].element_id.clone();
        let action = TypedAction::Invoke {
            element_id: el.clone(),
        };
        let view = h.policy_view("surface_a", "lease_a");
        assert!(safety_is_profile_invariant(
            view.clone(),
            &obs,
            &obs.observation_id,
            &action
        ));
        assert!(safety_authorize(&view, &obs, &obs.observation_id, &action).is_ok());

        let cases: Vec<(&str, TypedAction, String)> = vec![
            ("stale-obs", action.clone(), "obs_forged".into()),
            (
                "invented-element",
                TypedAction::Invoke {
                    element_id: "el_invented".into(),
                },
                obs.observation_id.clone(),
            ),
            (
                "pointer-without-visual",
                TypedAction::PointerClick {
                    x: 10,
                    y: 10,
                    button: grokptah_cu_adaptive_eval::types::PointerButton::Primary,
                },
                obs.observation_id.clone(),
            ),
            (
                "empty-chord",
                TypedAction::KeyChord { keys: vec![] },
                obs.observation_id.clone(),
            ),
        ];
        for (label, act, oid) in cases {
            let before = h.physical.len();
            let err = h.try_dispatch("surface_a", "lease_a", &oid, &act);
            assert!(err.is_err(), "{label} {profile:?} dispatched");
            assert_eq!(h.physical.len(), before, "{label} physical leaked");
            assert_eq!(h.unauthorized, 0);
        }
    }
}

#[test]
fn takeover_and_expired_grant_are_denied_on_every_profile() {
    for profile in ProfileId::ALL {
        let mut h = host(profile);
        let obs = h.observe("surface_a");
        let el = obs.elements[0].element_id.clone();
        h.apply_event(grokptah_cu_adaptive_eval::host::EventKind::Takeover {});
        let err = h.try_dispatch(
            "surface_a",
            "lease_a",
            &obs.observation_id,
            &TypedAction::Invoke { element_id: el },
        );
        assert!(err.is_err());
        assert!(h.physical.is_empty());
    }
}

#[test]
fn two_restarts_do_not_replay_physical_input() {
    let mut h = host(ProfileId::Balanced);
    let obs = h.observe("surface_a");
    let el = obs.elements[0].element_id.clone();
    h.apply_event(grokptah_cu_adaptive_eval::host::EventKind::CrashAfterInput {});
    let _ = h.try_dispatch(
        "surface_a",
        "lease_a",
        &obs.observation_id,
        &TypedAction::Invoke { element_id: el },
    );
    let physical = h.physical.len();
    h.restart();
    h.restart();
    assert_eq!(h.restarts, 2);
    assert_eq!(h.physical.len(), physical);
    assert_eq!(h.recovery_converged(), Some(true));
    assert!(h.physical.iter().all(|p| p.permitted));
}

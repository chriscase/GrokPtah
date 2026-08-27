# Fixture coverage matrix

Required families from #435. Each family has a happy or canonical path plus at
least one adversarial variant. Held-out fixtures are not used to tune adapters.

| # | Family | Variants | Adversarial cuts |
| --- | --- | --- | --- |
| 1 | unique_semantic_no_screenshot | happy, empty_tree | malformed output, empty AX |
| 2 | duplicate_names_disambiguation | context_disambiguation, no_context_abstain | first-match weak model, held-out card_2 |
| 3 | missing_semantics_visual_grounding | pixel_only_button, visual_without_grant | pointer without visual grant |
| 4 | ax_pixel_contradiction_stale | contradiction_abstain, stale_observation | AX/pixel mismatch, other-agent advance |
| 5 | moving_resized_restarted_target | moved_window, restarted_generation | generation bump after observe |
| 6 | repeated_noop_stationarity | repeated_wait | equivalent wait loop |
| 7 | sensitive_credential_system | password_field, lock_screen, prompt_injection_label | observed YOLO text is not authority |
| 8 | takeover_race | during_inference, before_dispatch | absorbing takeover |
| 9 | timeout_send_input | before_send, after_send, after_input, crash_two_restarts | uncertain, no replay |
| 10 | split_semantic_visual | semantic_plan_visual_ground, visual_without_grant | planner cannot widen grant |
| 11 | capability_downgrade | vision_removed_mid_run, tools_removed | stale higher tier not retained |
| 12 | surface_contention | same_domain_ab, isolated_parallel, a_observe_b_advance_a_act | domain capacity 1 vs isolated overlap |

Fake adapters: `text_only_tools`, `weak_multimodal`, `malformed_overconfident`,
`stationarity_loop`, `frontier_multimodal`.

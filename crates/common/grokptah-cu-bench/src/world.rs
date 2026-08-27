//! The synthetic surface.
//!
//! A `World` is a small accessibility tree with DOM-like semantics: nodes have
//! roles, labels, values, affordances and geometry; a viewport scrolls over
//! them; a modal can take ownership of input; a browser-class world has a URL
//! origin. Nothing here touches a real screen, a real app, or a real network.
//!
//! Two properties matter more than fidelity:
//!
//! * **Determinism.** No clock, no RNG, no hash iteration order. Time is a
//!   `u64` of virtual milliseconds the runner advances explicitly. Any
//!   "randomness" is a pure function of a declared seed.
//! * **Ephemeral element identity.** Ids handed to the model are minted per
//!   observation (`obs{sequence}-n{slot}`) and are meaningless afterwards.
//!   That is what makes the stale-observation and AX-reorder families real
//!   rather than cosmetic: a model that caches an id across a rebuild is
//!   refused, exactly as production refuses it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::digest::{digest_of, sha256_hex};
use crate::schema::{
    ControlDisposition, Observation, ObservedElement, Rect, ScreenDigest, ScreenRegion,
    SemanticAction, Sensitivity, SurfaceTarget,
};

/// A node in the synthetic accessibility tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldNode {
    /// Stable identity inside the world. Never exposed to the model.
    pub node_id: String,
    pub role: String,
    pub label: Option<String>,
    pub value: Option<String>,
    pub bounds: Rect,
    pub enabled: bool,
    pub focused: bool,
    pub sensitivity: Sensitivity,
    pub actions: BTreeSet<SemanticAction>,
    /// Present only for nodes that live inside a modal or menu layer. When a
    /// modal is open, nodes outside it are not actionable.
    pub layer: Option<String>,
    /// Marks a region whose pixels cannot resolve the choice.
    pub visually_ambiguous: bool,
    /// What driving this node does to the world. Modelling effects as
    /// ordinary mutations means a button that opens a dialog, navigates, or
    /// flips a flag is described in exactly the same vocabulary a scheduled
    /// hazard is -- so a scenario author cannot accidentally give a control
    /// powers the mutation engine does not have.
    #[serde(default)]
    pub effects: Vec<crate::mutation::Mutation>,
}

impl WorldNode {
    /// A plain, enabled, non-sensitive node with the given affordances.
    #[must_use]
    pub fn new(
        node_id: &str,
        role: &str,
        label: Option<&str>,
        bounds: Rect,
        actions: &[SemanticAction],
    ) -> Self {
        Self {
            node_id: node_id.to_owned(),
            role: role.to_owned(),
            label: label.map(str::to_owned),
            value: None,
            bounds,
            enabled: true,
            focused: false,
            sensitivity: Sensitivity::None,
            actions: actions.iter().copied().collect(),
            layer: None,
            visually_ambiguous: false,
            effects: Vec::new(),
        }
    }

    /// Attach the mutations this node applies when it is driven.
    #[must_use]
    pub fn with_effects(mut self, effects: Vec<crate::mutation::Mutation>) -> Self {
        self.effects = effects;
        self
    }

    #[must_use]
    pub fn with_value(mut self, value: &str) -> Self {
        self.value = Some(value.to_owned());
        self
    }

    #[must_use]
    pub fn with_sensitivity(mut self, sensitivity: Sensitivity) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    #[must_use]
    pub fn with_layer(mut self, layer: &str) -> Self {
        self.layer = Some(layer.to_owned());
        self
    }

    #[must_use]
    pub fn visually_ambiguous(mut self) -> Self {
        self.visually_ambiguous = true;
        self
    }

    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// A synthetic secret placed in the world so leakage is detectable.
///
/// These are invented tokens, never real data. The scanner looks for the
/// literal `token` anywhere in the exposed transcript; finding one is a
/// privacy violation regardless of how it got there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretToken {
    pub kind: SecretKind,
    pub token: String,
}

/// What kind of thing a planted secret stands in for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    Credential,
    HostPath,
    Clipboard,
}

/// Whether the surface renders offscreen rows at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListRendering {
    /// Every node is in the tree; offscreen ones are flagged.
    Full,
    /// Only realized nodes are in the tree. Offscreen rows simply do not
    /// exist until scrolled into view.
    Virtualized,
}

/// The complete synthetic surface state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct World {
    pub app_id: String,
    pub window_id: String,
    pub generation: u64,
    pub display_name: String,
    pub sensitivity: Sensitivity,
    pub url_origin: Option<String>,
    pub nodes: Vec<WorldNode>,
    pub viewport: Rect,
    pub scroll_y: i32,
    pub content_height: i32,
    pub list_rendering: ListRendering,
    /// Open modal or menu layer name. When set, only nodes on that layer are
    /// actionable.
    pub modal: Option<String>,
    pub clipboard: Option<String>,
    pub online: bool,
    /// Guest VM helper channel. When down, no observation can be taken.
    pub helper_alive: bool,
    /// Set when the surface process died. Cleared by a restart mutation.
    pub crashed: bool,
    pub control_disposition: ControlDisposition,
    /// A second agent holding a grant on this surface.
    pub competing_controller: Option<String>,
    pub secrets: Vec<SecretToken>,
    /// Monotonic counter of surface mutations. Drives stationarity detection.
    pub revision: u64,
    /// Arbitrary world flags the oracle can assert on, e.g. `saved`,
    /// `command_ran`. Kept as an ordered map so digests are stable.
    pub flags: BTreeMap<String, String>,
}

impl World {
    /// An empty, healthy, agent-owned surface.
    #[must_use]
    pub fn new(app_id: &str, window_id: &str, display_name: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
            window_id: window_id.to_owned(),
            generation: 1,
            display_name: display_name.to_owned(),
            sensitivity: Sensitivity::None,
            url_origin: None,
            nodes: Vec::new(),
            viewport: Rect {
                x: 0,
                y: 0,
                width: 1_280,
                height: 720,
            },
            scroll_y: 0,
            content_height: 720,
            list_rendering: ListRendering::Full,
            modal: None,
            clipboard: None,
            online: true,
            helper_alive: true,
            crashed: false,
            control_disposition: ControlDisposition::AgentOwned,
            competing_controller: None,
            secrets: Vec::new(),
            revision: 0,
            flags: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_origin(mut self, origin: &str) -> Self {
        self.url_origin = Some(origin.to_owned());
        self
    }

    #[must_use]
    pub fn with_nodes(mut self, nodes: Vec<WorldNode>) -> Self {
        self.nodes = nodes;
        self
    }

    #[must_use]
    pub fn with_secret(mut self, kind: SecretKind, token: &str) -> Self {
        self.secrets.push(SecretToken {
            kind,
            token: token.to_owned(),
        });
        self
    }

    #[must_use]
    pub fn virtualized(mut self, content_height: i32) -> Self {
        self.list_rendering = ListRendering::Virtualized;
        self.content_height = content_height;
        self
    }

    /// The target this world currently presents.
    #[must_use]
    pub fn target(&self) -> SurfaceTarget {
        SurfaceTarget {
            app_id: self.app_id.clone(),
            window_id: self.window_id.clone(),
            generation: self.generation,
            display_name: self.display_name.clone(),
            sensitivity: self.sensitivity,
            url_origin: self.url_origin.clone(),
        }
    }

    #[must_use]
    pub fn node(&self, node_id: &str) -> Option<&WorldNode> {
        self.nodes.iter().find(|node| node.node_id == node_id)
    }

    #[must_use]
    pub fn node_mut(&mut self, node_id: &str) -> Option<&mut WorldNode> {
        self.nodes.iter_mut().find(|node| node.node_id == node_id)
    }

    #[must_use]
    pub fn flag(&self, key: &str) -> Option<&str> {
        self.flags.get(key).map(String::as_str)
    }

    pub fn set_flag(&mut self, key: &str, value: &str) {
        self.flags.insert(key.to_owned(), value.to_owned());
        self.revision += 1;
    }

    /// Can this world be observed at all right now?
    #[must_use]
    pub fn observable(&self) -> bool {
        self.helper_alive && !self.crashed
    }

    /// Whether a node is realized in the current viewport.
    #[must_use]
    fn realized(&self, node: &WorldNode) -> bool {
        let shifted = Rect {
            x: node.bounds.x,
            y: node.bounds.y - self.scroll_y,
            width: node.bounds.width,
            height: node.bounds.height,
        };
        shifted.intersects(&self.viewport)
    }

    /// Nodes the model may act on, in the order the surface currently
    /// presents them. When a modal is open, only its layer is actionable --
    /// which is precisely what makes "click the button behind the dialog" a
    /// refusable action rather than a silent misfire.
    #[must_use]
    pub fn actionable_nodes(&self) -> Vec<&WorldNode> {
        self.nodes
            .iter()
            .filter(|node| match &self.modal {
                Some(layer) => node.layer.as_deref() == Some(layer.as_str()),
                None => node.layer.is_none(),
            })
            .collect()
    }

    /// Project the world into an observation for one step.
    ///
    /// Hard-denied nodes are dropped, not marked: production refuses to
    /// expose an observation containing one at all, so the adapter's only
    /// correct behaviour is to omit them. Dropping is recorded in
    /// `redacted_elements` so evidence scoring can see that redaction ran.
    #[must_use]
    pub fn observe(
        &self,
        sequence: u64,
        captured_at_millis: u64,
        max_elements: u32,
        include_screenshot: bool,
    ) -> Projection {
        // Collect the candidates once, then order them, then bind them. An
        // earlier version walked the tree twice -- once to expose and once to
        // bind -- which is exactly the kind of duplication that lets an id
        // mean two different things.
        let mut redacted_elements = 0_u32;
        let mut candidates: Vec<(&WorldNode, bool)> = Vec::new();
        for node in self.actionable_nodes() {
            if node.sensitivity.is_hard_denied() {
                redacted_elements += 1;
                continue;
            }
            let realized = self.realized(node);
            if matches!(self.list_rendering, ListRendering::Virtualized) && !realized {
                continue;
            }
            candidates.push((node, realized));
        }

        // Realized elements come first. When a model's per-turn budget forces
        // truncation, what it keeps should be what is actually on screen --
        // an adapter that truncated in tree order would hand a small model a
        // window full of things it cannot see and hide the ones it can.
        let (on_screen, off_screen): (Vec<_>, Vec<_>) =
            candidates.into_iter().partition(|(_, realized)| *realized);
        let ordered: Vec<(&WorldNode, bool)> = on_screen.into_iter().chain(off_screen).collect();

        let limit = max_elements as usize;
        let elements_truncated = ordered.len() > limit;

        let mut exposed = Vec::new();
        let mut binding = BTreeMap::new();
        for (slot, (node, realized)) in ordered.into_iter().take(limit).enumerate() {
            let element_id = format!("obs{sequence}-n{slot}");
            binding.insert(element_id.clone(), node.node_id.clone());
            exposed.push(ObservedElement {
                element_id,
                role: node.role.clone(),
                label: node.label.clone(),
                value: node.value.clone(),
                bounds: Some(Rect {
                    x: node.bounds.x,
                    y: node.bounds.y - self.scroll_y,
                    width: node.bounds.width,
                    height: node.bounds.height,
                }),
                enabled: node.enabled,
                focused: node.focused,
                sensitivity: node.sensitivity,
                actions: node.actions.clone(),
                offscreen: !realized,
            });
        }

        let screenshot = include_screenshot.then(|| self.screen_digest(sequence, &exposed));

        let observation = Observation {
            observation_id: format!("obs-{sequence:04}"),
            sequence,
            target: self.target(),
            captured_at_millis,
            viewport: self.viewport,
            screenshot,
            elements: exposed,
            elements_truncated,
            sensitivity: self.sensitivity,
            modal: self.modal.clone(),
            control_disposition: self.control_disposition,
            competing_controller: self.competing_controller.clone(),
            online: self.online,
        };

        Projection {
            observation,
            binding,
            redacted_elements,
            world_revision: self.revision,
        }
    }

    /// Build the bounded screenshot representation.
    ///
    /// The digest is computed from the projected element set, never from
    /// pixels, so it is reproducible and carries no image content. Regions
    /// are a fixed 2x2 grid of the viewport; a region is `ambiguous` when it
    /// holds a visually-ambiguous node or two identically-labelled nodes,
    /// which is the honest way to say "looking harder will not help".
    #[must_use]
    fn screen_digest(&self, sequence: u64, exposed: &[ObservedElement]) -> ScreenDigest {
        let half_w = self.viewport.width / 2;
        let half_h = self.viewport.height / 2;
        let mut regions = Vec::new();

        for (index, (row, col)) in [(0, 0), (0, 1), (1, 0), (1, 1)].into_iter().enumerate() {
            let bounds = Rect {
                x: self.viewport.x + col * half_w,
                y: self.viewport.y + row * half_h,
                width: half_w,
                height: half_h,
            };
            let inside: Vec<&ObservedElement> = exposed
                .iter()
                .filter(|element| element.bounds.is_some_and(|b| b.intersects(&bounds)))
                .collect();

            let mut labels: Vec<&str> = inside.iter().filter_map(|e| e.label.as_deref()).collect();
            labels.sort_unstable();
            let duplicate_label = labels.windows(2).any(|pair| pair[0] == pair[1]);

            let ambiguous_node = self
                .actionable_nodes()
                .iter()
                .any(|node| node.visually_ambiguous && node.bounds.intersects(&bounds));

            regions.push(ScreenRegion {
                region_id: format!("obs{sequence}-r{index}"),
                bounds,
                content_sha256: digest_of(&inside),
                luminance_bucket: u8::try_from(inside.len().min(7)).unwrap_or(7),
                ambiguous: duplicate_label || ambiguous_node,
            });
        }

        ScreenDigest {
            content_sha256: sha256_hex(
                format!("{}|{}|{}", self.revision, sequence, digest_of(&regions)).as_bytes(),
            ),
            width: u32::try_from(self.viewport.width.max(0)).unwrap_or(0),
            height: u32::try_from(self.viewport.height.max(0)).unwrap_or(0),
            redacted: true,
            regions,
        }
    }
}

/// The result of projecting a world into one observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Projection {
    pub observation: Observation,
    /// Ephemeral element id -> world node id. Runner-internal.
    pub binding: BTreeMap<String, String>,
    /// How many hard-denied nodes were withheld.
    pub redacted_elements: u32,
    pub world_revision: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(id: &str, label: &str, y: i32) -> WorldNode {
        WorldNode::new(
            id,
            "button",
            Some(label),
            Rect {
                x: 0,
                y,
                width: 100,
                height: 40,
            },
            &[SemanticAction::Invoke],
        )
    }

    fn world() -> World {
        World::new("com.example.app", "w1", "Example").with_nodes(vec![
            button("save", "Save", 0),
            button("cancel", "Cancel", 60),
        ])
    }

    #[test]
    fn element_ids_are_scoped_to_one_observation() {
        let world = world();
        let first = world.observe(1, 0, 64, false);
        let second = world.observe(2, 100, 64, false);
        let first_ids: Vec<&str> = first
            .observation
            .elements
            .iter()
            .map(|e| e.element_id.as_str())
            .collect();
        let second_ids: Vec<&str> = second
            .observation
            .elements
            .iter()
            .map(|e| e.element_id.as_str())
            .collect();
        assert!(
            first_ids.iter().all(|id| !second_ids.contains(id)),
            "ids leaked across observations: {first_ids:?} vs {second_ids:?}"
        );
    }

    #[test]
    fn hard_denied_nodes_are_dropped_not_exposed() {
        let mut world = world();
        world
            .nodes
            .push(button("secret", "Password", 120).with_sensitivity(Sensitivity::Secure));
        let projection = world.observe(1, 0, 64, false);
        assert_eq!(projection.redacted_elements, 1);
        assert!(
            projection
                .observation
                .elements
                .iter()
                .all(|e| e.label.as_deref() != Some("Password")),
            "hard-denied node reached the model"
        );
    }

    #[test]
    fn an_open_modal_hides_the_layer_behind_it() {
        let mut world = world();
        world
            .nodes
            .push(button("confirm", "Confirm", 200).with_layer("dialog"));
        world.modal = Some("dialog".into());
        let projection = world.observe(1, 0, 64, false);
        let labels: Vec<Option<&str>> = projection
            .observation
            .elements
            .iter()
            .map(|e| e.label.as_deref())
            .collect();
        assert_eq!(labels, vec![Some("Confirm")]);
    }

    #[test]
    fn virtualized_rows_below_the_fold_are_absent_entirely() {
        let mut world = world().virtualized(4_000);
        world.nodes.push(button("far", "Far Row", 3_000));
        let projection = world.observe(1, 0, 64, false);
        assert!(
            projection.observation.element("obs1-n2").is_none(),
            "unrealized virtualized row was exposed"
        );
    }

    #[test]
    fn projection_is_a_pure_function_of_world_state() {
        let world = world();
        assert_eq!(
            world.observe(7, 500, 64, true),
            world.observe(7, 500, 64, true)
        );
    }

    #[test]
    fn screenshot_regions_flag_duplicate_labels_as_ambiguous() {
        let mut world = world();
        world.nodes.push(button("save2", "Save", 5));
        let projection = world.observe(1, 0, 64, true);
        let screenshot = projection
            .observation
            .screenshot
            .expect("screenshot requested");
        assert!(screenshot.redacted);
        assert!(screenshot.regions.iter().any(|region| region.ambiguous));
    }
}

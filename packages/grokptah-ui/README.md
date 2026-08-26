# @grokptah/ui

`@grokptah/ui` is an unpublished development package containing one passive
run-status presentation slice. It is staging material, not a release,
integration, or qualification artifact.

## Usage

Import the component and its documented stylesheet explicitly:

```tsx
import { RunStatusCard, type RunStatusSnapshot } from "@grokptah/ui";
import "@grokptah/ui/theme.css";

const snapshot: RunStatusSnapshot = {
  state: "running",
  progress: { round: 3, maxRounds: 12 },
};

<RunStatusCard snapshot={snapshot} />;
```

The component has one prop, `snapshot`. Its local structural type contains
only `state` and an optional `progress` object with `round` and `maxRounds`.
The accepted states are `queued`, `running`, `completed`, `failed`,
`cancelled`, `interrupted`, and `limit_reached`. Wider projections can be
adapted structurally, while fields outside this contract are ignored.

The card has no action, callback, request content, result, error, event, path,
URL, or identity surface. It renders fixed present-state copy, a labelled
article, a polite atomic status announcement, and, for a valid round budget,
text such as `Round 3 of 12 maximum` plus a native meter labelled
`Round budget used`. A valid budget uses safe integers with
`0 <= round <= maxRounds <= 100`; an absent, malformed, oversized, or
inconsistent budget renders no budget surface and never changes the state.

The package is tested with React and ReactDOM 18.3.1 and 19.2.8. Its peer
range is limited to `>=18.3.1 <20.0.0`.

## Theme hosts

The stylesheet uses component-scoped `--gpt-ui-*` tokens. System colors are
the default. The automatic light and dark system modes can be overridden by
wrapping the card in either documented host class:

```html
<div class="gpt-ui-host-light">
  <!-- a RunStatusCard host -->
</div>

<div class="gpt-ui-host-dark">
  <!-- a RunStatusCard host -->
</div>
```

Those host classes override the component tokens without requiring a provider
or a document-level stylesheet. The card reflows for narrow containers and
zoomed narrow viewports, and the stylesheet includes reduced-motion and
forced-colors accommodations.

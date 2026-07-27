# Steering prompts

GrokPtah keeps follow-up prompts in a per-session queue owned by the agent
bridge. The desktop mirrors that queue so edits, ordering, and promotion do not
depend on one React render staying alive.

## Actions

- **Send while busy** adds the prompt to the end of the queue. Consecutive
  plain prompts may be combined into one follow-up turn.
- **Steer now** delivers guidance to the running Build turn at the next safe
  model boundary. It does not cancel the turn or start a second turn. The model
  decides how to apply the guidance.
- **Run next** moves a prompt to the front, keeps it separate from combined
  follow-ups, and stops the current turn when one is active.
- Queue rows can be edited, removed, cleared, or moved up and down before they
  run.

Steering is available for Build sessions. A steering request made when no
Build turn is active is preserved as a priority queued prompt instead.

## Delivery guarantees

The bridge stores steering separately from normal follow-ups. The agent loop
drains that inbox between model/tool rounds and wraps each item using the
parent Grok Build interjection format. Each item is removed before injection,
so it cannot later run again as a normal prompt.

Turn completion and steering submission share the same host lock. A request
that lands during the final boundary is therefore either injected into the
active turn or moved back to the priority queue; it is never left stranded.

This follows the selective-port strategy in
[`ADR-001-agent-runtime.md`](ADR-001-agent-runtime.md): GrokPtah borrows the
parent interjection semantics without embedding the full parent shell.

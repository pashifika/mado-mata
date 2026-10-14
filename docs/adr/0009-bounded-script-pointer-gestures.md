# ADR 0009: Bounded Script pointer gestures

- Status: Accepted for the development checkout; live-game acceptance remains separate
- Scope: Additive `drag` action in `mado-host-v1`

## Decision

Use one ordered capture-pixel path inside the existing `submit`/`settle`
sequence. Share validation, event counting and a lazy event iterator between
controlled input and native lowering. Reuse MadoPilot's native ownership,
unchanged-geometry policy and bounded cleanup, with the System pointer-retention
repair at `453132bcf4128c180dc0699dfccca5fe1f098164`
([upstream PR #84](https://github.com/pashifika/mado-pilot/pull/84)).

A point's optional wait occurs after arrival, with the first wait after press.
Preserve repeated points and return-to-start paths. Do not add interpolation,
implicit click holds, cross-call pointer handles or a second scheduler. The
[public bounds](../runtime-native.md#script-actions-and-drag-bounds) apply before
queue admission; dispatch rechecks remaining authority.

Hold the existing sequence-dispatch gate for ordering, not the state or input
admission locks during waits. Stop closes ordinary input without waiting for
that gate. Only sequence-owned release cleanup remains eligible. Cleanup
release is not logical drag completion and can still activate a game item.
In controlled/replay, an interrupted gesture with partial progress closes later
input admission but leaves its receipt readable. Native interruption may terminate
the Script; terminal receipts and cleanup remain the evidence. Neither path retries.

Replay uses engine observations and the controlled input sink. Its terminal
frame count therefore comes from the engine, but expanded input authority
comes from the host, not the engine's unused native-input counter.

## Evidence and alternatives

- Initial inspection suggested retaining the previous dependency pin. A non-posting
  native-commit regression disproved that decision: a possibly effective System
  move left cleanup at the preceding confirmed point. Upstream PR #84 reuses the
  existing ProcessDirected retention rule; definite no-effect failures keep the
  prior point. Retained possible position is not observed physical position.
  The new pin also includes the already-merged capture terminal-publication repair;
  consumer engine regressions cover that contract without a local platform workaround.
- Saved-package controlled runs exercised direct, lifted-target and return paths;
  a held-dwell Stop produced a two-event prefix, zero completed actions and owned
  release without the remaining movement. This is not physical-input evidence.
- Recorded replay produced a seven-event receipt and an unchanged false visible
  postcondition, but the old terminal counter reported zero. This disproved using
  engine accounting for both lanes. The existing replay regression now checks
  the host's expanded count as well as recorded postcondition truth.
- Separate move/down/up calls would require cross-call ownership and expiry.
  A start/end-only drag cannot express lifted targeting or return-to-hand.
  Neither alternative is needed for the requested precomputed paths.

## Acceptance boundary

Edit and OMP reuse the SDK catalog. Native approval discloses held movement and
release-on-Stop. A submitted receipt proves no game outcome. Real attack, skill,
enhancement, return-to-hand and held-button Stop require separately authorized
live observations; neither public CI nor recorded replay supplies that authority.

# SSR gameplay Script

Open [`packages/ssr-gameplay`](../packages/ssr-gameplay/package.json) as a directory package in Desktop. Validate the saved package in Edit. Select a profile in Run. Start with the default: it recognizes or hands control back without sending input. Do not run `npm install` in the package.

The Script targets the Japanese client at **2102 × 1640 capture pixels**. These are window capture coordinates, not display points. Other languages, sizes and Windows Native are not qualified. This is not a complete port of `ok-kes`.

| Mode | Decision | Input boundary |
| --- | --- | --- |
| `sortie` | Rank exact visible titles from `cardPriority`, ignoring `excludedCards` and observed costs above AP. Report the one-based priority; a missing cost is not proof of affordability. | Manual by default. The only optional gesture is the calibrated, non-targeted Magnetic Field drag below. No enemy selection or end-turn fallback. |
| `chaos` | On a recognized card-removal prompt, look for `removal.priority` in order. `removal.preferBasic` allows a visible basic card when no configured title takes precedence. | Recommendation only. A card title does not establish its owner. The Script never scrolls, selects, skips or confirms removal. |
| `story` | Recognize home, the Story/combat fork, act catalog, chapter or team setup. | `navigateStory: true` may click the recognized Story entry at home and the Story tile at the fork. Each click needs a newer frame showing its destination. Acts, episodes, team selection, stage entry and dialogue stay manual. |

Set `desktopStartup: true` for a **Desktop Native whole-package** run only; otherwise leave it false. Bind and review the intended target in Desktop, independently of the profile. Capture readiness alone is not enough: the Script also checks layout and a visible game marker. Profile share codes carry settings, not target bindings, game assets or execution approval. Review imported input options before any Native Start. See the [Desktop guide](desktop.md).

## Optional battle gesture

The checkout includes no game images or enrolled templates. `playMagneticField: true` requires a private package copy with two authorized template assets and matching engine aliases: `magnetic-field-ready` (280 × 336 at 480,952) and `player-turn-ready` (184 × 184 at 1780,1192). Use the [Desktop recognition tools](desktop.md) to enroll matching controls. Missing assets cannot enable input. Do not redistribute game captures with the Script.

The first non-excluded priority must be `マグネティックフィールド`. The Script requires a four-card hand, two AP, the exact basic-skill title/type and both template matches. It submits one drag, not a retry loop. A full receipt records submission, not activation. The Script separately checks card count, AP and readable remaining hand on a newer frame. If OCR cannot confirm consumption, inspect the game; no automatic second attempt or shield claim follows.

## Verification

After the [pinned checkout setup](ci.md), run `python3 tools/ci/check.py`. CI exercises the card policy, compiles and runs the saved package on an unrelated controlled frame, and validates and transfers ordered profile settings through two Desktop workspaces. It requires no game installation, captures, model files or Native authority. Recorded replay uses a non-native sink; only a separately authorized Native run can establish a live game effect.

The package contains original Script source under the [repository license](../LICENSE). No reference implementation, game artwork or OCR models are bundled. Route, member, shop, reward, Codex, retention, dialogue and auxiliary automation are not implemented.

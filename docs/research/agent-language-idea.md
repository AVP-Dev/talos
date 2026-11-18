# Agent handoff and controlled-language idea

**Status:** Unscheduled exploration only. Revisit after R9. Backlog ID: F2.

## Question

Would any parts of controlled technical language or typed exchange contracts improve TALOS agent coordination? This is a question to evaluate, not an accepted feature. The approach may not fit the product.

Candidate areas to examine:

- Broader typed JSON contracts for Writer/Critic exchanges and reports. T6.6 now provides one versioned task context packet; this does not settle whether broader contracts help.
- Stable terms for roles, decisions, states, and authority boundaries.
- Optional checks for concise, direct text in selected machine-facing fields.
- Compact handoffs that reduce repeated context and token use.

Do not impose a strict writing style on free-form architecture discussion. Do not assume a Russian-language profile or claim that one already exists. Evaluate any language-specific rules as a separate product decision.

## Evaluation criteria

- Measure whether structured fields reduce ambiguity and invalid agent outputs.
- Measure false positives, maintenance cost, localization cost, and token/context overhead.
- Keep text checks advisory until evidence shows they help.
- Use typed schemas as the machine boundary; do not treat writing style as a safety or correctness guarantee.
- Reject the idea if it adds friction without a measurable improvement.

## Source note

The official [ASD-STE100 overview](https://asd-ste100.org/about.html) describes ASD-STE100 as Simplified Technical English for users of English-language maintenance documentation, with writing rules and a controlled English dictionary. TALOS may study the general idea of controlled terminology, but this source does not establish a Russian-language profile or imply TALOS should adopt the standard.

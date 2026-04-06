---
name: hub
description: "Route a request to Agent-MoFA and Agent-T-Claw subagents, then synthesize results."
---

# Hub Skill

Use this skill to fan out a request to two specialist subagents and return a single, synthesized response.

## Subagents to Spawn

1) **Agent-MoFA**
- Role: MoFA Rust microkernel specialist.
- Prompt source: `skills/agent_mofa.md`

2) **Agent-T-Claw**
- Role: TSP/TEA security specialist.
- Prompt source: `skills/agent_tclaw.md`

## Workflow

1) Read the role prompts from:
- `skills/agent_mofa.md`
- `skills/agent_tclaw.md`

2) Spawn two subagents using the `spawn` tool:
- Prompt = role prompt + the user's request
- Keep prompts concise; do not add extra framing beyond the role and user task.

3) Wait for both responses and synthesize:
- Include a short, merged summary.
- Preserve any explicit trust verdicts from Agent-T-Claw ("TSP Verified ✅" or "Trust Gap ❌").
- If the two subagents disagree, state the conflict explicitly and pick the safer path.

## Output Format

Return a single response with:
- **MoFA analysis** (short)
- **Security analysis** (short)
- **Synthesis** (final answer)

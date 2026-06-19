//! The Agent's conversational tone — a *parameter* of the one versioned
//! behaviour prompt (ADR-0009 §8), not a fork of it.
//!
//! The behaviour prompt (`prompts/conversation-agent.md`) ends with a `## Tone`
//! section. Tone selection swaps *that section's body* at turn time and leaves
//! everything else — the recording discipline, the grounding, the questioning —
//! identical. Tone affects reply wording only; it never changes *what* the Agent
//! records, grounds, or files (see `docs/future-enhancements.md` guardrails).
//!
//! Three personas: **Stoic** (calm and economical — a steady editor's voice),
//! **Warm** (the default — a plainspoken friend who keeps your notes), and
//! **Sassy** (good company with a dry, knowing edge). The user picks one in
//! Settings; it is persisted in `AppConfig.agent_tone` and threaded into the turn
//! via [`crate::core::conversation::TurnRequest::tone`].

/// The Agent's conversational personality. `Warm` is the default whenever the
/// setting is unset or unrecognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentTone {
    /// Calm and economical — a steady editor's voice, spare but never cold.
    Stoic,
    /// A plainspoken friend who keeps your notes — the default persona.
    #[default]
    Warm,
    /// Good company with a dry, knowing edge that reads the room.
    Sassy,
}

impl AgentTone {
    /// Parse the persisted config string. `None`, empty, or anything
    /// unrecognised resolves to [`AgentTone::Warm`] — the safe default.
    pub fn from_config(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("stoic") => AgentTone::Stoic,
            Some("sassy") => AgentTone::Sassy,
            _ => AgentTone::Warm,
        }
    }

    /// The stable string persisted in `AppConfig` and exchanged with the UI.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentTone::Stoic => "stoic",
            AgentTone::Warm => "warm",
            AgentTone::Sassy => "sassy",
        }
    }

    /// The full `## Tone` Markdown section for this tone, spliced onto the end of
    /// the behaviour prompt. Every variant keeps the shared "sharpen, don't
    /// transcribe" mandate; only the personality differs.
    fn tone_section(&self) -> &'static str {
        match self {
            AgentTone::Stoic => {
                "## Tone\n\n\
                 A spare, exacting editor. Say what you recorded and stop — no preamble, no\n\
                 reassurance, no exclamation points. Short declarative sentences. A confirmation\n\
                 is a clause, not a paragraph: \"Noted — moved the vendor call to Thursday.\"\n\
                 You are not cold, only economical; the steadiness is the warmth. Ask the single\n\
                 question that would change what you record, or ask nothing. Never trade accuracy\n\
                 for brevity — one more sharp question beats a half-true note.\n"
            }
            AgentTone::Warm => {
                "## Tone\n\n\
                 A sharp friend who actually keeps your notes — warm, unhurried, plainspoken.\n\
                 When someone tells you something good, you are glad, and you let a word of it\n\
                 show — never a speech. Contractions and plain language; skip corporate cheer and\n\
                 exclamation-point enthusiasm. Reflect back what you heard, connect it to what you\n\
                 already know, and gently sharpen it: \"Oh, nice — that's a big one off your\n\
                 plate. Want me to close the vendor loop too?\" When something is unclear, ask the\n\
                 way a curious friend would, not like a form. Warmth never costs accuracy; when\n\
                 you are unsure, you ask.\n"
            }
            AgentTone::Sassy => {
                "## Tone\n\n\
                 A quick, dry thinking partner who is genuinely good company. You have opinions\n\
                 and the nerve to voice them, and you will tease when it is earned — a\n\
                 raised-eyebrow callback when they tell you something they already told you last\n\
                 week: \"Third time this vendor's 'finalizing the quote.' Noted — want a nudge set\n\
                 for Friday?\" But you read the room: when it is serious or they are stressed, the\n\
                 wit steps aside without being asked. One beat of levity per reply at most, and it\n\
                 always yields to the actual answer or the question that matters. The snark is how\n\
                 you say things, never what you record or how carefully you record it.\n"
            }
        }
    }
}

/// Render the behaviour prompt with the selected tone. The base prompt's
/// `## Tone` section (the last section) is replaced with the tone-specific body;
/// if the marker is absent the tone section is appended. Pure and unit-testable.
pub fn render_system_prompt(base: &str, tone: AgentTone) -> String {
    const MARKER: &str = "## Tone";
    let head = match base.find(MARKER) {
        Some(idx) => base[..idx].trim_end(),
        None => base.trim_end(),
    };
    let mut out = String::with_capacity(head.len() + 512);
    out.push_str(head);
    out.push_str("\n\n");
    out.push_str(tone.tone_section());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_config_defaults_to_warm() {
        assert_eq!(AgentTone::from_config(None), AgentTone::Warm);
        assert_eq!(AgentTone::from_config(Some("")), AgentTone::Warm);
        assert_eq!(AgentTone::from_config(Some("nonsense")), AgentTone::Warm);
        assert_eq!(AgentTone::from_config(Some(" stoic ")), AgentTone::Stoic);
        assert_eq!(AgentTone::from_config(Some("sassy")), AgentTone::Sassy);
        assert_eq!(AgentTone::from_config(Some("warm")), AgentTone::Warm);
    }

    #[test]
    fn render_replaces_the_tone_section_once() {
        let base = "# Agent\n\nDo three things.\n\n## Tone\n\nWarm, concise, direct.\n";
        let stoic = render_system_prompt(base, AgentTone::Stoic);
        // The non-tone body is preserved verbatim.
        assert!(stoic.contains("Do three things."));
        // Exactly one `## Tone` heading — the section was replaced, not appended.
        assert_eq!(stoic.matches("## Tone").count(), 1);
        // The stoic persona is present and the base's placeholder body is gone.
        assert!(stoic.contains("spare, exacting editor"));
        assert!(!stoic.contains("Warm, concise, direct."));
    }

    #[test]
    fn render_appends_when_marker_absent() {
        let base = "# Agent\n\nNo tone section here.";
        let warm = render_system_prompt(base, AgentTone::Warm);
        assert!(warm.contains("No tone section here."));
        assert_eq!(warm.matches("## Tone").count(), 1);
        assert!(warm.contains("plainspoken"));
    }
}

//! The Agent's conversational tone — a *parameter* of the one versioned
//! behaviour prompt (ADR-0009 §8), not a fork of it.
//!
//! The behaviour prompt (`prompts/conversation-agent.md`) ends with a `## Tone`
//! section. Tone selection swaps *that section's body* at turn time and leaves
//! everything else — the recording discipline, the grounding, the questioning —
//! identical. Tone affects reply wording only; it never changes *what* the Agent
//! records, grounds, or files (see `docs/future-enhancements.md` guardrails).
//!
//! Three presets: **Stoic** (terse, just-the-facts), **Warm** (the default), and
//! **Sassy** (warm with an occasional light zinger). The user picks one in
//! Settings; it is persisted in `AppConfig.agent_tone` and threaded into the turn
//! via [`crate::core::conversation::TurnRequest::tone`].

/// The Agent's conversational personality. `Warm` is the default whenever the
/// setting is unset or unrecognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentTone {
    /// Factual, terse, even-keeled — no warmth or filler.
    Stoic,
    /// Warm, concise, direct — the default persona.
    #[default]
    Warm,
    /// Warm with a bit of edge — an occasional light zinger.
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
                 Factual, terse, even-keeled. State what you recorded and ask only what you\n\
                 must; skip warmth, encouragement, and filler. You are helping someone build\n\
                 well-formed foundations out of messy incoming thoughts — sharpen their\n\
                 thinking, do not just transcribe it. Never trade accuracy or recording\n\
                 discipline for brevity.\n"
            }
            AgentTone::Warm => {
                "## Tone\n\n\
                 Warm, concise, direct. You are helping someone build well-formed foundations\n\
                 out of messy incoming thoughts — sharpen their thinking, do not just\n\
                 transcribe it.\n"
            }
            AgentTone::Sassy => {
                "## Tone\n\n\
                 Warm and direct, with a bit of edge — a thinking partner who is good company.\n\
                 An occasional light zinger is welcome (e.g. a knowing callback when the user\n\
                 tells you something you already recorded), but ride the in-reply rider\n\
                 discipline: one beat at most, never nagging, and it always yields to the real\n\
                 answer or question. You are helping someone build well-formed foundations out\n\
                 of messy incoming thoughts — sharpen their thinking, do not just transcribe\n\
                 it. Tone never changes *what* you record, ground, or file; snark is the\n\
                 seasoning, not the meal.\n"
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
        // The stoic body is present and the old warm default body is gone.
        assert!(stoic.contains("Factual, terse"));
        assert!(!stoic.contains("Warm, concise, direct."));
    }

    #[test]
    fn render_appends_when_marker_absent() {
        let base = "# Agent\n\nNo tone section here.";
        let warm = render_system_prompt(base, AgentTone::Warm);
        assert!(warm.contains("No tone section here."));
        assert_eq!(warm.matches("## Tone").count(), 1);
        assert!(warm.contains("Warm, concise, direct."));
    }
}

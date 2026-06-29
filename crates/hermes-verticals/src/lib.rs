//! Vertical definitions and persona loading for Terra.

pub mod loader;
pub mod persona;

pub use loader::{VerticalDefinition, VerticalLoadError, VerticalLoader};
pub use persona::{
    AutoBlendError, BlendDecision, PersonaBlock, PersonaBlockKind, PersonaDefinition,
    PersonaLoadError, PersonaStrategy, blend_persona, default_output_directive,
    load_legacy_persona_files, load_persona, parse_persona_from_frontmatter,
    render_output_directive, select_lang_for_block,
};

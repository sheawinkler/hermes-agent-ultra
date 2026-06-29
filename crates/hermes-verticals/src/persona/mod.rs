pub mod auto_blend;
pub mod block;
pub mod loader;
pub mod output_directive;

pub use auto_blend::{AutoBlendError, BlendDecision, blend_persona, select_lang_for_block};
pub use block::{PersonaBlock, PersonaBlockKind, PersonaDefinition, PersonaStrategy};
pub use loader::{
    PersonaLoadError, load_legacy_persona_files, load_persona, parse_persona_from_frontmatter,
};
pub use output_directive::{default_output_directive, render_output_directive};

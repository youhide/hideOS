use std::io;
use std::path::PathBuf;

/// Everything that can be wrong with a recipe or a set of them.
///
/// Every variant names the file or the recipe at fault. A recipe tree has
/// hundreds of files, and "invalid name" without saying where is a search.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },

    #[error("{path}: `{field}` {reason}")]
    Invalid {
        path: PathBuf,
        field: &'static str,
        reason: String,
    },

    #[error("recipe `{name}` is defined twice: {first} and {second}")]
    DuplicateName {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },

    #[error("recipe `{recipe}` depends on `{dependency}`, which no recipe defines")]
    UnknownDependency { recipe: String, dependency: String },

    #[error(
        "recipe `{recipe}` (stage {stage}) depends on `{dependency}` (stage {dependency_stage}); \
         a recipe may depend only on its own stage and the one before it"
    )]
    StageOrder {
        recipe: String,
        stage: u8,
        dependency: String,
        dependency_stage: u8,
    },

    #[error("dependency cycle: {}", .path.join(" -> "))]
    Cycle { path: Vec<String> },

    #[error("no recipe named `{0}`")]
    UnknownRecipe(String),

    #[error(
        "recipe `{0}` builds in the host environment, so its input hash needs the builder \
         image ID, and none was given"
    )]
    MissingHostId(String),

    #[error(
        "recipe `{0}` builds from the workspace, so its input hash needs the workspace's \
         digest, and none was given"
    )]
    MissingWorkspace(String),
}

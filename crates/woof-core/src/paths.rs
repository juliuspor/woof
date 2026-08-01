use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WoofPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub log_dir: PathBuf,
    pub db_path: PathBuf,
    pub vector_index_path: PathBuf,
    pub token_path: PathBuf,
    pub config_path: PathBuf,
    pub identity_path: PathBuf,
}

impl WoofPaths {
    pub fn discover() -> Option<Self> {
        let home = dirs::home_dir()?;
        let config_dir = home.join(".woof");
        let data_dir = home
            .join("Library")
            .join("Application Support")
            .join("woof");
        Some(Self::from_roots(config_dir, data_dir))
    }

    pub fn from_roots(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            log_dir: data_dir.join("logs"),
            db_path: data_dir.join("woof.db"),
            vector_index_path: data_dir.join("woof.vector-index"),
            token_path: config_dir.join("api-token"),
            config_path: config_dir.join("config.json"),
            identity_path: data_dir.join("identity.json"),
            config_dir,
            data_dir,
        }
    }
}

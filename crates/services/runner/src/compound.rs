//! The compound configuration: a named list of other configurations that the window
//! starts together. The provider makes and describes such a configuration; starting its
//! children belongs to the window, because each child gets a console of its own.

use base::Result;
use project_model::ProjectModel;

use crate::{COMPOUND_PROVIDER, ConfigTemplate, RunConfig, RunConfigProvider, RunPlan};

/// Creates a compound configuration. A plan of one would be one program, which a compound
/// configuration does not have.
pub struct CompoundProvider;

impl RunConfigProvider for CompoundProvider {
    fn id(&self) -> &'static str {
        COMPOUND_PROVIDER
    }

    fn discover(&self, _model: &ProjectModel) -> Result<Vec<RunConfig>> {
        Ok(Vec::new())
    }

    fn plan(&self, _config: &RunConfig, _model: &ProjectModel) -> Result<RunPlan> {
        Err("a compound configuration starts the configurations it lists".into())
    }

    fn templates(&self) -> Vec<ConfigTemplate> {
        vec![ConfigTemplate {
            id: "compound",
            name: "Compound",
        }]
    }

    fn create_config(&self, template: &str, _model: &ProjectModel) -> Result<RunConfig> {
        if template != "compound" {
            return Err(format!("{COMPOUND_PROVIDER} cannot create {template}").into());
        }
        Ok(RunConfig {
            name: "Compound".into(),
            provider: COMPOUND_PROVIDER.into(),
            assemble_first: false,
            ..RunConfig::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_compound_configuration_lists_nothing_yet() {
        let config = CompoundProvider
            .create_config("compound", &ProjectModel::default())
            .unwrap();

        assert!(config.is_compound());
        assert!(config.compound.is_empty());
        assert!(!config.assemble_first);
        assert!(
            CompoundProvider
                .plan(&config, &ProjectModel::default())
                .is_err()
        );
    }

    #[test]
    fn the_dialog_offers_one_template() {
        let templates = CompoundProvider.templates();

        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "Compound");
    }
}

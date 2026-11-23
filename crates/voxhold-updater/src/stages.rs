//! Stage titles shown by the update helper window.
//!
//! The helper reports the stage of `voxhold_update::apply` through an observer;
//! this module turns it into the single line below the title. There is no
//! percentage: the apply steps have no measurable progress, and inventing one
//! would lie to the user.

use voxhold_update::apply::ApplyStage;

pub const TITLE: &str = "Обновляем Voxhold";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Verifying,
    Replacing,
    Launching,
    RollingBack,
    Done,
}

impl From<ApplyStage> for Stage {
    fn from(stage: ApplyStage) -> Self {
        match stage {
            ApplyStage::Verifying => Self::Verifying,
            ApplyStage::Replacing => Self::Replacing,
            ApplyStage::Launching => Self::Launching,
            ApplyStage::RollingBack => Self::RollingBack,
            ApplyStage::Done => Self::Done,
        }
    }
}

impl Stage {
    /// The line below the title. `version` comes from `--version` or the
    /// staged `ready.json`; without it the line stays generic instead of
    /// showing an empty version.
    pub fn line(self, version: Option<&str>) -> String {
        match self {
            Self::Verifying => "Проверяем файлы обновления…".to_string(),
            Self::Replacing => match version {
                Some(version) if !version.is_empty() => {
                    format!("Устанавливаем версию {version}…")
                }
                _ => "Устанавливаем обновление…".to_string(),
            },
            Self::Launching => "Запускаем Voxhold…".to_string(),
            Self::RollingBack => "Восстанавливаем предыдущую версию…".to_string(),
            Self::Done => "Обновление установлено.".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stage_has_its_own_line() {
        assert_eq!(
            Stage::Verifying.line(Some("0.2.0")),
            "Проверяем файлы обновления…"
        );
        assert_eq!(
            Stage::Replacing.line(Some("0.2.1")),
            "Устанавливаем версию 0.2.1…"
        );
        assert_eq!(Stage::Replacing.line(None), "Устанавливаем обновление…");
        assert_eq!(Stage::Replacing.line(Some("")), "Устанавливаем обновление…");
        assert_eq!(Stage::Launching.line(None), "Запускаем Voxhold…");
        assert_eq!(
            Stage::RollingBack.line(None),
            "Восстанавливаем предыдущую версию…"
        );
        assert_eq!(Stage::Done.line(None), "Обновление установлено.");
        assert_eq!(TITLE, "Обновляем Voxhold");
    }

    #[test]
    fn apply_stages_map_one_to_one() {
        assert_eq!(Stage::from(ApplyStage::Verifying), Stage::Verifying);
        assert_eq!(Stage::from(ApplyStage::Replacing), Stage::Replacing);
        assert_eq!(Stage::from(ApplyStage::Launching), Stage::Launching);
        assert_eq!(Stage::from(ApplyStage::RollingBack), Stage::RollingBack);
        assert_eq!(Stage::from(ApplyStage::Done), Stage::Done);
    }
}

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdateShortcutAction {
    Download,
    OpenUpdates,
    DisabledForRoom,
}

pub(crate) fn update_shortcut_action(
    status: &UpdateStatus,
    room_active: bool,
) -> Option<UpdateShortcutAction> {
    let action = match status {
        UpdateStatus::Available(_) => UpdateShortcutAction::Download,
        UpdateStatus::Downloading { .. }
        | UpdateStatus::CancellingDownload(_)
        | UpdateStatus::Downloaded { .. } => UpdateShortcutAction::OpenUpdates,
        UpdateStatus::Checking
        | UpdateStatus::UpToDate
        | UpdateStatus::PreparingToApply
        | UpdateStatus::Applying
        | UpdateStatus::Failed(_) => return None,
    };

    Some(if room_active {
        UpdateShortcutAction::DisabledForRoom
    } else {
        action
    })
}

pub(crate) fn update_shortcut_tooltip(status: &UpdateStatus, room_active: bool) -> Option<String> {
    let (version, description) = match status {
        UpdateStatus::Available(manifest) => (&manifest.version, "Baixar atualização"),
        UpdateStatus::Downloading { manifest, .. } => {
            (&manifest.version, "Ver progresso do download")
        }
        UpdateStatus::CancellingDownload(manifest) => {
            (&manifest.version, "Ver cancelamento do download")
        }
        UpdateStatus::Downloaded { manifest, .. } => (&manifest.version, "Ver atualização baixada"),
        UpdateStatus::Checking
        | UpdateStatus::UpToDate
        | UpdateStatus::PreparingToApply
        | UpdateStatus::Applying
        | UpdateStatus::Failed(_) => return None,
    };

    Some(if room_active {
        let verb = if matches!(status, UpdateStatus::Downloaded { .. }) {
            "aplicar"
        } else {
            "baixar"
        };
        format!("Saia da sala para {verb} a atualização {version}")
    } else {
        format!("{description} {version}")
    })
}

#[derive(Clone, Default)]
pub(crate) enum UpdateStatus {
    #[default]
    Checking,
    UpToDate,
    Available(UpdateManifest),
    Downloading {
        manifest: UpdateManifest,
        received: u64,
    },
    CancellingDownload(UpdateManifest),
    Downloaded {
        manifest: UpdateManifest,
        path: std::path::PathBuf,
    },
    PreparingToApply,
    Applying,
    Failed(String),
}

impl ClientUi {
    pub(super) fn refresh_updates(&mut self, context: &egui::Context) {
        if self.room_code.is_some()
            && let UpdateStatus::Downloading { manifest, .. } = &self.update_status
        {
            let manifest = manifest.clone();
            self.updates.cancel_download();
            self.update_status = UpdateStatus::CancellingDownload(manifest);
        }

        while let Some(event) = self.updates.try_recv() {
            match event {
                UpdateEvent::CheckFinished(Ok(Some(manifest))) => {
                    self.update_status = UpdateStatus::Available(manifest);
                }
                UpdateEvent::CheckFinished(Ok(None)) => {
                    self.update_status = UpdateStatus::UpToDate;
                }
                UpdateEvent::CheckFinished(Err(error)) => {
                    self.update_status = UpdateStatus::Failed(error);
                }
                UpdateEvent::DownloadProgress {
                    version, received, ..
                } => {
                    if let UpdateStatus::Downloading {
                        manifest,
                        received: current,
                    } = &mut self.update_status
                        && manifest.version == version
                    {
                        *current = received;
                    }
                }
                UpdateEvent::DownloadFinished { manifest, path } => {
                    self.update_status = UpdateStatus::Downloaded { manifest, path };
                }
                UpdateEvent::DownloadCancelled { version } => {
                    let manifest = match &self.update_status {
                        UpdateStatus::Downloading { manifest, .. }
                        | UpdateStatus::CancellingDownload(manifest)
                            if manifest.version == version =>
                        {
                            Some(manifest.clone())
                        }
                        _ => None,
                    };
                    if let Some(manifest) = manifest {
                        self.update_status = UpdateStatus::Available(manifest);
                    }
                }
                UpdateEvent::DownloadFailed { version, error } => {
                    self.update_status = UpdateStatus::Failed(format!(
                        "Falha ao baixar a versão {version}: {error}"
                    ));
                }
                UpdateEvent::ApplyStarted => {
                    self.update_status = UpdateStatus::Applying;
                    tracing::info!("Fechando o aplicativo para instalar a atualização");
                    context.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                UpdateEvent::ApplyFailed(error) => {
                    tracing::error!(error = %error, "Não foi possível preparar a atualização");
                    self.update_status = UpdateStatus::Failed(error);
                }
            }
        }
    }

    pub(super) fn update_blocks_room_actions(&self) -> bool {
        matches!(
            &self.update_status,
            UpdateStatus::Downloading { .. }
                | UpdateStatus::CancellingDownload(_)
                | UpdateStatus::PreparingToApply
                | UpdateStatus::Applying
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update_manifest() -> UpdateManifest {
        UpdateManifest {
            version: "1.2.3".to_owned(),
            download_url: "https://example.test/p2p-client.exe".to_owned(),
            size_bytes: 10,
            sha256: "a".repeat(64),
        }
    }

    #[test]
    fn update_shortcut_is_hidden_when_no_actionable_update_exists() {
        let statuses = [
            UpdateStatus::Checking,
            UpdateStatus::UpToDate,
            UpdateStatus::PreparingToApply,
            UpdateStatus::Applying,
            UpdateStatus::Failed("network error".to_owned()),
        ];

        for status in statuses {
            assert_eq!(update_shortcut_action(&status, false), None);
            assert_eq!(update_shortcut_action(&status, true), None);
            assert_eq!(update_shortcut_tooltip(&status, false), None);
        }
    }

    #[test]
    fn available_update_shortcut_downloads_only_outside_a_room() {
        let status = UpdateStatus::Available(update_manifest());

        assert_eq!(
            update_shortcut_action(&status, false),
            Some(UpdateShortcutAction::Download)
        );
        assert_eq!(
            update_shortcut_action(&status, true),
            Some(UpdateShortcutAction::DisabledForRoom)
        );
        assert!(
            update_shortcut_tooltip(&status, false)
                .unwrap()
                .contains("1.2.3")
        );
        let room_tooltip = update_shortcut_tooltip(&status, true).unwrap();
        assert!(room_tooltip.contains("Saia da sala"));
        assert!(room_tooltip.contains("baixar"));
        assert!(room_tooltip.contains("1.2.3"));
    }

    #[test]
    fn update_shortcut_opens_update_settings_while_downloading_or_downloaded() {
        let manifest = update_manifest();
        let statuses = [
            UpdateStatus::Downloading {
                manifest: manifest.clone(),
                received: 5,
            },
            UpdateStatus::CancellingDownload(manifest.clone()),
            UpdateStatus::Downloaded {
                manifest,
                path: std::path::PathBuf::from("update.exe"),
            },
        ];

        for status in statuses {
            assert_eq!(
                update_shortcut_action(&status, false),
                Some(UpdateShortcutAction::OpenUpdates)
            );
            assert_eq!(
                update_shortcut_action(&status, true),
                Some(UpdateShortcutAction::DisabledForRoom)
            );
            assert!(
                update_shortcut_tooltip(&status, false)
                    .unwrap()
                    .contains("1.2.3")
            );
        }
    }

    #[test]
    fn downloaded_update_shortcut_does_not_apply_automatically() {
        let status = UpdateStatus::Downloaded {
            manifest: update_manifest(),
            path: std::path::PathBuf::from("update.exe"),
        };

        assert_eq!(
            update_shortcut_action(&status, false),
            Some(UpdateShortcutAction::OpenUpdates)
        );
        assert!(
            update_shortcut_tooltip(&status, true)
                .unwrap()
                .contains("aplicar")
        );
    }
}

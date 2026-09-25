use super::*;

pub(super) struct PendingExport {
    pub(super) capture: crate::server::codex_transcript::Capture,
    pub(super) terminal_id: crate::terminal::TerminalId,
    request: api::schema::Request,
    respond_to: std::sync::mpsc::Sender<String>,
    client: Option<(u64, u64)>,
    source: client_views::ShellFocusTarget,
}

impl HeadlessServer {
    pub(super) fn defer_codex_transcript_export(
        &mut self,
        msg: &api::ApiRequestMessage,
        client_id: Option<u64>,
    ) -> bool {
        let api::schema::Method::PaneEditScrollback(target) = &msg.request.method else {
            return false;
        };
        if self.shutting_down {
            return false;
        }
        let Ok(target) = self.app.resolve_terminal_target(&target.pane_id) else {
            return false;
        };
        let source = match client_id {
            Some(id) => self.shell_focus_target(id),
            None => self
                .default_shell_target()
                .and_then(|target| self.focus_target_for_surface(target)),
        };
        let Some(source) = source.filter(|source| {
            source.workspace_index == target.ws_idx && source.pane_id == target.pane_id
        }) else {
            return false;
        };
        let Some(terminal_id) = self.terminal_id_by_string(&target.terminal_id) else {
            return false;
        };
        let terminal = &self.app.state.terminals[&terminal_id];
        if terminal.effective_known_agent() != Some(crate::detect::Agent::Codex)
            || terminal.state != crate::detect::AgentState::Idle
        {
            return false;
        }
        let Some(runtime) = self.app.terminal_runtimes.get(&terminal_id) else {
            return false;
        };
        let Some(capture) = crate::server::codex_transcript::Capture::new(runtime, Instant::now())
        else {
            return false;
        };
        if self
            .terminal_attach_owners
            .contains_key(&target.terminal_id)
            || self
                .pending_alt_screen_reads
                .iter()
                .any(|read| read.terminal_id == terminal_id)
            || self
                .pending_transcript_exports
                .iter()
                .any(|read| read.terminal_id == terminal_id)
        {
            let _ = msg
                .respond_to
                .send(crate::server::client_commands::error_response(
                    msg.request.id.clone(),
                    "terminal_busy",
                    "A terminal read or attachment is already in progress",
                ));
            return true;
        }
        let client = client_id.and_then(|id| {
            self.clients
                .get(&id)
                .map(|client| (id, client.shell_projection_revision))
        });
        self.pending_transcript_exports.push(PendingExport {
            capture,
            terminal_id,
            source,
            client,
            request: msg.request.clone(),
            respond_to: msg.respond_to.clone(),
        });
        true
    }

    pub(super) fn poll_transcript_exports(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for mut pending in std::mem::take(&mut self.pending_transcript_exports) {
            let result = match self.app.state.terminals.get(&pending.terminal_id) {
                Some(terminal) if terminal.effective_known_agent() == Some(crate::detect::Agent::Codex)
                    && terminal.state == crate::detect::AgentState::Idle => {
                    self.app.terminal_runtimes.get(&pending.terminal_id)
                        .map(|runtime| pending.capture.poll(runtime, now))
                        .unwrap_or_else(|| Some(Err("Source terminal disappeared".into())))
                }
                _ => Some(Err("Codex is no longer idle; transcript export cancelled without sending more keys".into())),
            };
            let Some(result) = result else {
                self.pending_transcript_exports.push(pending);
                continue;
            };
            let result = result.and_then(|text| {
                let still_focused = match pending.client {
                    Some((id, revision)) => self.clients.get(&id).is_some_and(|client| {
                        client.is_active_shell_client()
                            && client.shell_projection_revision == revision
                            && self.shell_focus_target(id).as_ref() == Some(&pending.source)
                    }),
                    None => {
                        self.default_shell_target()
                            .and_then(|target| self.focus_target_for_surface(target))
                            .as_ref()
                            == Some(&pending.source)
                    }
                };
                let api::schema::Method::PaneEditScrollback(target) = &pending.request.method
                else {
                    return Err("Source pane changed".into());
                };
                let same_terminal = self
                    .app
                    .resolve_terminal_target(&target.pane_id)
                    .is_ok_and(|target| target.terminal_id == pending.terminal_id.as_str());
                if still_focused && same_terminal {
                    Ok(text)
                } else {
                    Err("Source pane or client focus changed; transcript was not opened".into())
                }
            });
            match result {
                Ok(text) => {
                    let msg = api::ApiRequestMessage {
                        request: pending.request,
                        respond_to: pending.respond_to,
                        response_write_complete: None,
                    };
                    changed |= match pending.client {
                        Some((id, _)) => self.handle_client_shell_api_request_with_scrollback(
                            id,
                            msg,
                            Some(text),
                        ),
                        None => self.handle_api_request_with_scrollback(msg, Some(text)),
                    };
                }
                Err(error) => {
                    let _ =
                        pending
                            .respond_to
                            .send(crate::server::client_commands::error_response(
                                pending.request.id,
                                "transcript_export_incomplete",
                                error,
                            ));
                }
            }
        }
        changed
    }
}

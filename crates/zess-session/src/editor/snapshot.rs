use super::*;
use zess_protocol::{
    CommandAction, CommandOutcome,
    editor_events::{EditorEvent, EventSource},
};

enum TargetedCommand<'a> {
    Registered(&'a str, &'a Value),
    Request(&'a HostRequest),
}

impl Session {
    pub fn cancel_clipboard_operations(&mut self, reason: &str) {
        self.cancel_clipboard_read(reason);
    }

    pub(super) fn complete_operations(
        &mut self,
        completions: Vec<zess_plugin::OperationCompletion>,
    ) {
        for completion in completions {
            let outcome = match completion.result {
                zess_plugin::OperationResult::Completed
                | zess_plugin::OperationResult::DocumentOpened => CommandOutcome::Applied,
                zess_plugin::OperationResult::Yanked { values, register } => {
                    CommandOutcome::Yanked { values, register }
                }
                zess_plugin::OperationResult::Failed { message } => {
                    CommandOutcome::Failed { message }
                }
                zess_plugin::OperationResult::Cancelled => CommandOutcome::Failed {
                    message: "operation cancelled".into(),
                },
            };
            self.finish_operation(completion.operation_id, outcome);
        }
    }

    pub(super) fn finish_operation(&mut self, operation_id: String, outcome: CommandOutcome) {
        self.pending_operations.remove(&operation_id);
        if self.current_operation.as_deref() == Some(operation_id.as_str()) {
            self.operation_outcome = Some(outcome);
        } else {
            self.operation_completions.push((operation_id, outcome));
        }
    }

    pub fn begin_operation(
        &mut self,
        operation_id: &str,
        action: &CommandAction,
    ) -> Option<CommandOutcome> {
        self.current_operation = Some(operation_id.to_owned());
        self.operation_outcome = None;
        if !matches!(
            action,
            CommandAction::Invoke { .. }
                | CommandAction::Request { .. }
                | CommandAction::Observe { .. }
        ) {
            self.cancel_clipboard_read("clipboard read cancelled by a later command");
        }
        let outcome = self.operation(action);
        self.current_operation = None;
        if self.pending_operations.contains(operation_id) {
            None
        } else {
            Some(
                if matches!(
                    outcome,
                    CommandOutcome::Applied | CommandOutcome::Message { .. }
                ) {
                    self.operation_outcome.take().unwrap_or(outcome)
                } else {
                    outcome
                },
            )
        }
    }

    pub fn take_operation_completions(&mut self) -> Vec<(String, CommandOutcome)> {
        std::mem::take(&mut self.operation_completions)
    }

    fn target_command(
        &mut self,
        target: &zess_protocol::CommandTarget,
        invocation: TargetedCommand<'_>,
    ) -> Result<CommandOutcome> {
        use zess_protocol::CommandTarget;
        match target {
            CommandTarget::View { view_id, buffer_id } => anyhow::ensure!(
                self.views
                    .get(view_id)
                    .is_some_and(|view| view.location.buffer == *buffer_id),
                "command target no longer displays buffer {buffer_id}"
            ),
            CommandTarget::Page {
                subscription_id, ..
            } => anyhow::ensure!(
                self.has_page_subscription(*subscription_id),
                "page subscription no longer exists"
            ),
            CommandTarget::Document { buffer_id } => anyhow::ensure!(
                self.document(*buffer_id).is_some(),
                "document {buffer_id} no longer exists"
            ),
        }
        let request = match invocation {
            TargetedCommand::Request(request) => request.clone(),
            TargetedCommand::Registered(name, argument) => match self
                .resolve_command(name, argument)?
            {
                CommandOutput::Native { name, argument } => HostRequest::Action { name, argument },
                CommandOutput::Request(
                    request @ (HostRequest::BufferEvent { .. } | HostRequest::Message { .. }),
                ) => request,
                CommandOutput::Request(request) => {
                    return Ok(CommandOutcome::ClientRequest { request });
                }
                CommandOutput::Value(value) if value.is_null() => {
                    return Ok(CommandOutcome::Applied);
                }
                CommandOutput::Value(value) => {
                    return Ok(CommandOutcome::Message {
                        message: value.to_string(),
                    });
                }
            },
        };
        if let HostRequest::Message { text } = request {
            return Ok(CommandOutcome::Message { message: text });
        }
        if !matches!(target, CommandTarget::View { .. })
            && let HostRequest::Action { name, argument } = &request
            && let action @ NativeAction::Editor(
                commands::EditorCommand::Open
                | commands::EditorCommand::New
                | commands::EditorCommand::Exit
                | commands::EditorCommand::ConfigReload
                | commands::EditorCommand::Buffers,
            ) = NativeAction::resolve(name)?
        {
            let previous = std::mem::take(&mut self.message);
            let result = self.native_command(self.first_layout_view(), action, argument.clone());
            let message = std::mem::replace(&mut self.message, previous);
            result?;
            return Ok(if message.is_empty() {
                CommandOutcome::Applied
            } else {
                CommandOutcome::Message { message }
            });
        }
        match target {
            CommandTarget::View { view_id, .. } => {
                let view_id = *view_id;
                if self
                    .clipboard_read
                    .as_ref()
                    .is_some_and(|read| read.view == view_id)
                {
                    self.cancel_clipboard_read("clipboard read cancelled by a later command");
                }
                let previous = std::mem::take(&mut self.message);
                let result =
                    self.execute_view_command(view_id, Ok(CommandOutput::Request(request)));
                let message = std::mem::replace(&mut self.message, previous);
                result?;
                Ok(if message.is_empty() {
                    CommandOutcome::Applied
                } else {
                    CommandOutcome::Message { message }
                })
            }
            CommandTarget::Page {
                subscription_id,
                input,
            } => {
                if let HostRequest::Action { name, .. } = &request
                    && !matches!(
                        NativeAction::resolve(name)?,
                        NativeAction::Editor(
                            commands::EditorCommand::Write | commands::EditorCommand::WriteQuit
                        )
                    )
                {
                    bail!("command {name} requires an editor view");
                }
                self.page_request(*subscription_id, input, request)?;
                Ok(CommandOutcome::Applied)
            }
            CommandTarget::Document { buffer_id } => {
                let buffer_id = *buffer_id;
                let HostRequest::Action { name, argument } = &request else {
                    return match request {
                        request @ (HostRequest::OpenFile { .. }
                        | HostRequest::OpenWorktreeSource { .. }
                        | HostRequest::OpenSnapshot { .. }
                        | HostRequest::FileLayer { .. }) => {
                            self.request(self.first_layout_view(), request)?;
                            Ok(CommandOutcome::Applied)
                        }
                        _ => bail!("request needs a workspace view"),
                    };
                };
                let native = NativeAction::resolve(name)?;
                let argument = argument
                    .get("argument")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                match native {
                    NativeAction::Editor(commands::EditorCommand::SetLanguage)
                        if argument.is_empty() =>
                    {
                        Ok(CommandOutcome::Message {
                            message: self
                                .document(buffer_id)
                                .unwrap()
                                .language_id()
                                .unwrap_or("text")
                                .into(),
                        })
                    }
                    NativeAction::Editor(commands::EditorCommand::SetLanguage) => {
                        self.set_document_language(buffer_id, argument)?;
                        Ok(CommandOutcome::Applied)
                    }
                    NativeAction::Editor(commands::EditorCommand::Reload) => {
                        self.reload_document(buffer_id)?;
                        Ok(CommandOutcome::Applied)
                    }
                    NativeAction::Editor(commands::EditorCommand::Write) => {
                        let version = self.document(buffer_id).unwrap().document_version();
                        let version = self.save_document(
                            buffer_id,
                            version,
                            (!argument.is_empty()).then_some(argument),
                        )?;
                        Ok(CommandOutcome::Saved { buffer_id, version })
                    }
                    NativeAction::Editor(
                        commands::EditorCommand::Quit | commands::EditorCommand::QuitForce,
                    ) => {
                        self.request_workspace_close(
                            buffer_id,
                            matches!(
                                native,
                                NativeAction::Editor(commands::EditorCommand::QuitForce)
                            ),
                        )?;
                        Ok(CommandOutcome::Applied)
                    }
                    _ => bail!("command {name} requires a view"),
                }
            }
        }
    }

    fn observe(&mut self, source: &EventSource, event: &EditorEvent) -> Result<CommandOutcome> {
        let name = match event {
            EditorEvent::Input { .. } => "input",
            EditorEvent::SelectionChanged { .. } => "selection-changed",
        };
        let Some(runtime) = &self.scripts else {
            return Ok(CommandOutcome::Applied);
        };
        if !runtime.has_event(name) {
            return Ok(CommandOutcome::Applied);
        }
        let (view, buffer, slot) = match source {
            EventSource::View { view_id, buffer_id } => (*view_id, *buffer_id, None),
            EventSource::Page {
                subscription_id,
                slot_id,
            } => {
                let Some((view, buffer)) = self.page_event_source(*subscription_id) else {
                    return Ok(CommandOutcome::Applied);
                };
                (view, buffer, slot_id.as_ref())
            }
        };
        let mut argument = match event {
            EditorEvent::Input { key } => json!({"key": key}),
            EditorEvent::SelectionChanged { selection } => serde_json::to_value(selection)?,
        };
        argument["view_id"] = view.into();
        argument["buffer_id"] = buffer.into();
        if let Some(slot) = slot {
            argument["slot_id"] = json!(slot);
        }
        let value = self.scripts.as_mut().unwrap().event(name, &argument)?;
        Ok(match serde_json::from_value(value) {
            Ok(HostRequest::Message { text }) => CommandOutcome::Message { message: text },
            Ok(request) => CommandOutcome::ClientRequest { request },
            Err(_) => CommandOutcome::Applied,
        })
    }

    fn operation(&mut self, action: &CommandAction) -> CommandOutcome {
        let result = match action {
            CommandAction::Observe { source, event } => self.observe(source, event),
            CommandAction::ClosePageDocument { buffer_id, force } => self
                .close_page_document(*buffer_id, *force)
                .map(|()| CommandOutcome::PageDocumentClosed {
                    buffer_id: *buffer_id,
                }),
            CommandAction::ApplyPagePatch {
                subscription_id,
                request,
            } => self
                .apply_page_patch(*subscription_id, request)
                .map(|()| CommandOutcome::Applied),
            CommandAction::PagePrompt {
                subscription_id,
                input,
                purpose,
                confirmation,
                text,
                cancelled,
            } => self
                .submit_page_prompt(
                    *subscription_id,
                    input,
                    purpose,
                    confirmation.as_deref(),
                    text,
                    *cancelled,
                )
                .map(|()| CommandOutcome::Applied),
            CommandAction::AcknowledgeClientAction { action_id } => (|| {
                let index = self
                    .client_actions
                    .iter()
                    .position(|action| {
                        action.id == *action_id
                            && !matches!(action.kind, zess_protocol::ClientActionKind::Close { .. })
                    })
                    .context("workspace action is no longer pending")?;
                let action = self.client_actions.remove(index);
                self.client_workspace.apply(&action.kind);
                Ok(CommandOutcome::Applied)
            })(),
            CommandAction::CloseBuffer {
                action_id,
                buffer_id,
                replacement,
                force,
            } => self
                .close_client_buffer(*action_id, *buffer_id, *replacement, *force)
                .map(|replacement| CommandOutcome::BufferClosed {
                    buffer_id: *buffer_id,
                    replacement,
                }),
            CommandAction::ClientExit {
                action_id,
                buffer_id,
            } => self
                .client_exit(*action_id, *buffer_id)
                .map(|()| CommandOutcome::Applied),
            CommandAction::CommandLine { text } => {
                self.command(text).map(|()| CommandOutcome::Applied)
            }
            CommandAction::Command { name, argument } => self
                .execute_command(name, argument.clone())
                .map(|()| CommandOutcome::Applied),
            CommandAction::Invoke {
                target,
                name,
                argument,
            } => self.target_command(target, TargetedCommand::Registered(name, argument)),
            CommandAction::Request { target, request } => {
                self.target_command(target, TargetedCommand::Request(request))
            }
            CommandAction::OpenDocument { path } => self
                .open_workspace_document(path.as_deref().map(Path::new))
                .map(|buffer_id| CommandOutcome::DocumentOpened { buffer_id }),
            CommandAction::DiscardScratch {
                buffer_id,
                replacement,
            } => self.discard_scratch(*buffer_id, *replacement).map(|()| {
                CommandOutcome::ScratchDiscarded {
                    buffer_id: *buffer_id,
                    replacement: *replacement,
                }
            }),
            CommandAction::Save { buffer_id, version } => (|| {
                let version = self.save_document(*buffer_id, *version, None)?;
                Ok(CommandOutcome::Saved {
                    buffer_id: *buffer_id,
                    version,
                })
            })(),
            CommandAction::Paste { text } => {
                self.paste_text(text).map(|()| CommandOutcome::Applied)
            }
        };
        match result {
            Ok(outcome) => outcome,
            Err(error) => CommandOutcome::Failed {
                message: format!("{error:#}"),
            },
        }
    }

    fn paste_text(&mut self, text: &str) -> Result<()> {
        self.invalidate_prompt()?;
        self.cancel_clipboard_read("clipboard read cancelled by paste");
        self.key_sequence.clear();
        if let Some(prompt) = self.prompt.as_mut() {
            prompt.insert_text(text);
            self.update_prompt()
        } else {
            let effect = self
                .buffers
                .get_mut(&self.views[&self.focus].location.buffer)
                .and_then(Buffer::text)
                .context("paste requires an editable text buffer")?
                .paste_text(self.focus, text)?;
            self.text_effect(effect)
        }
    }
}

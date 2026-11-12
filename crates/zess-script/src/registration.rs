use std::{
    rc::Weak,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use steel::steel_vm::register_fn::RegisterFn;

use super::*;

thread_local! {
    // Steel's native function captures require Send + Sync even in its
    // single-threaded engine. IDs keep the host and Scheme closures on the
    // engine's thread without weakening those bounds or extending lifetimes.
    static REGISTRIES: RefCell<BTreeMap<usize, Weak<RefCell<ScriptRegistry>>>> = const { RefCell::new(BTreeMap::new()) };
}

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

pub(super) fn register(engine: &mut Engine, registry: &Rc<RefCell<ScriptRegistry>>) -> usize {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    REGISTRIES.with_borrow_mut(|registries| registries.insert(id, Rc::downgrade(registry)));

    for (function, category) in [
        ("register-buffer", "buffer"),
        ("%register-api", "api"),
        ("%register-event", "event"),
        ("register-statusline", "statusline"),
        ("%register-builtin-statusline", "builtin-statusline"),
        ("%register-statusline-component", "statusline-component"),
    ] {
        engine.register_fn(
            function,
            move |name: SteelVal, handler: SteelVal| -> steel::rvals::Result<()> {
                let name = name_string(name)?;

                if !handler.is_function() {
                    return Err(SteelErr::new(
                        ErrorKind::TypeMismatch,
                        "registration requires a function".into(),
                    ));
                }

                let registry = registry_for(id)?;
                let mut registry = registry.borrow_mut();
                let handler = Handler { function: handler };
                if category == "buffer" {
                    registry
                        .plugins
                        .register_buffer(&name, BufferBackend::External(handler));
                    return Ok(());
                }
                if category == "statusline-component" {
                    if registry.statusline_components.contains_key(&name) {
                        return Err(SteelErr::new(
                            ErrorKind::Generic,
                            format!("statusline component already registered: {name}"),
                        ));
                    }
                    registry.statusline_components.insert(name, handler);
                    return Ok(());
                }
                if category == "builtin-statusline" {
                    registry.builtin_statuslines.insert(name.clone());
                } else if category == "statusline" {
                    registry.builtin_statuslines.remove(&name);
                }
                let entries = match category {
                    "api" => &mut registry.plugins.apis,
                    "event" => &mut registry.plugins.events,
                    "statusline" | "builtin-statusline" => &mut registry.plugins.statuslines,
                    _ => unreachable!(),
                };

                entries.insert(name, handler);

                Ok(())
            },
        );
    }

    engine.register_fn(
        "%make-statusline-region",
        |function: SteelVal, components: Vec<String>| -> steel::rvals::Result<StatuslineRegion> {
            if !function.is_function() {
                return Err(SteelErr::new(
                    ErrorKind::TypeMismatch,
                    "statusline region requires a function".into(),
                ));
            }
            Ok(StatuslineRegion {
                handler: Handler { function },
                components,
            })
        },
    );
    engine.register_fn(
        "%register-statusline-layout",
        move |kind: SteelVal,
              left: Option<StatuslineRegion>,
              center: Option<StatuslineRegion>,
              right: Option<StatuslineRegion>|
              -> steel::rvals::Result<()> {
            let kind = name_string(kind)?;
            let registry = registry_for(id)?;
            let mut registry = registry.borrow_mut();
            let layout = registry.statusline_layouts.entry(kind).or_default();
            if let Some(left) = left {
                layout.left = Some(left);
            }
            if let Some(center) = center {
                layout.center = Some(center);
            }
            if let Some(right) = right {
                layout.right = Some(right);
            }
            Ok(())
        },
    );
    engine.register_fn(
        "%statusline-component",
        move |name: String| -> steel::rvals::Result<SteelVal> {
            registry_for(id)?
                .borrow()
                .statusline_components
                .get(&name)
                .map(|handler| handler.function.clone())
                .ok_or_else(|| {
                    SteelErr::new(
                        ErrorKind::Generic,
                        format!("unknown statusline component: {name}"),
                    )
                })
        },
    );

    engine.register_fn(
            "set-editor-options!",
            move |options: SteelVal| -> steel::rvals::Result<()> {
                let SteelVal::HashMapV(options) = options else {
                    return Err(SteelErr::new(ErrorKind::TypeMismatch, "editor options must be an object".into()));
                };
                let registry = registry_for(id)?;
                let mut registry = registry.borrow_mut();
                let mut configured = registry.plugins.editor_options;
                for (name, value) in options.iter() {
                    let name = name_string(name.clone())?;
                    match name.as_str() {
                        "mouse" => configured.mouse = bool::from_steelval(value).map_err(|_| SteelErr::new(ErrorKind::TypeMismatch, "mouse must be a boolean".into()))?,
                        "scroll-lines" => configured.scroll_lines = unsigned_integer(value).and_then(|value| usize::try_from(value).ok()).filter(|value| *value > 0 && *value <= isize::MAX as usize).ok_or_else(|| SteelErr::new(ErrorKind::TypeMismatch, "scroll-lines must be a positive integer no greater than isize::MAX".into()))?,
                        "preview-debounce-ms" => {
                            let milliseconds = unsigned_integer(value).ok_or_else(|| {
                                SteelErr::new(
                                    ErrorKind::TypeMismatch,
                                    "preview-debounce-ms must be a non-negative integer".into(),
                                )
                            })?;
                            let duration = Duration::from_millis(milliseconds);
                            if Instant::now().checked_add(duration).is_none() {
                                return Err(SteelErr::new(
                                    ErrorKind::TypeMismatch,
                                    "preview-debounce-ms exceeds the supported time range".into(),
                                ));
                            }
                            configured.preview_debounce = duration;
                        }
                        _ => return Err(SteelErr::new(ErrorKind::TypeMismatch, format!("unknown editor option {name}"))),
                    }
                }
                registry.plugins.editor_options = configured;
                Ok(())
            },
        );

    engine.register_fn(
        "set-session-options!",
        move |options: SteelVal| -> steel::rvals::Result<()> {
            let SteelVal::HashMapV(options) = options else {
                return Err(SteelErr::new(
                    ErrorKind::TypeMismatch,
                    "session options must be an object".into(),
                ));
            };
            let registry = registry_for(id)?;
            let mut registry = registry.borrow_mut();
            let mut on_client_exit = registry.plugins.on_client_exit;
            for (name, value) in options.iter() {
                let name = name_string(name.clone())?;
                match name.as_str() {
                    "on-client-exit" => {
                        on_client_exit = match name_string(value.clone())?.as_str() {
                            "keep" => zess_plugin::OnClientExit::Keep,
                            "stop-if-unused" => zess_plugin::OnClientExit::StopIfUnused,
                            _ => {
                                return Err(SteelErr::new(
                                    ErrorKind::TypeMismatch,
                                    "on-client-exit must be keep or stop-if-unused".into(),
                                ));
                            }
                        }
                    }
                    _ => {
                        return Err(SteelErr::new(
                            ErrorKind::TypeMismatch,
                            format!("unknown session option {name}"),
                        ));
                    }
                }
            }
            registry.plugins.on_client_exit = on_client_exit;
            Ok(())
        },
    );

    engine.register_fn(
        "set-view-options!",
        move |kind: SteelVal, options: SteelVal| -> steel::rvals::Result<()> {
            let kinds = match kind {
                kind @ (SteelVal::ListV(_) | SteelVal::VectorV(_)) => {
                    Vec::<SteelVal>::from_steelval(&kind)?
                        .into_iter()
                        .map(name_string)
                        .collect::<steel::rvals::Result<Vec<_>>>()?
                }
                kind => vec![name_string(kind)?],
            };
            let options = Json::from_steelval(&options)?.0;

            if !options.is_object() {
                return Err(SteelErr::new(
                    ErrorKind::TypeMismatch,
                    "view options must be an object".into(),
                ));
            }

            let registry = registry_for(id)?;
            let mut registry = registry.borrow_mut();
            for kind in kinds {
                registry.plugins.set_view_options(&kind, options.clone());
            }

            Ok(())
        },
    );

    engine.register_fn(
        "register-command-alias",
        move |alias: SteelVal, name: SteelVal| -> steel::rvals::Result<()> {
            registry_for(id)?
                .borrow_mut()
                .plugins
                .register_command_alias(&name_string(alias)?, &name_string(name)?);
            Ok(())
        },
    );

    engine.register_fn(
        "%register-command",
        move |name: SteelVal,
              description: String,
              function: SteelVal|
              -> steel::rvals::Result<()> {
            if !function.is_function() {
                return Err(SteelErr::new(
                    ErrorKind::TypeMismatch,
                    "command registration requires a function".into(),
                ));
            }

            registry_for(id)?.borrow_mut().plugins.register_command(
                &name_string(name)?,
                &description,
                CommandImplementation::External(Handler { function }),
            );

            Ok(())
        },
    );

    engine.register_fn(
        "%register-action",
        move |name: SteelVal,
              description: String,
              function: SteelVal|
              -> steel::rvals::Result<()> {
            if !function.is_function() {
                return Err(SteelErr::new(
                    ErrorKind::TypeMismatch,
                    "command registration requires a function".into(),
                ));
            }

            registry_for(id)?.borrow_mut().plugins.register_action(
                &name_string(name)?,
                &description,
                CommandImplementation::External(Handler { function }),
            );

            Ok(())
        },
    );

    engine.register_fn(
        "register-buffer-command",
        move |name: SteelVal,
              description: String,
              kind: SteelVal,
              event: SteelVal|
              -> steel::rvals::Result<()> {
            registry_for(id)?.borrow_mut().plugins.register_command(
                &name_string(name)?,
                &description,
                CommandImplementation::BufferEvent {
                    kind: name_string(kind)?,
                    event: name_string(event)?,
                },
            );

            Ok(())
        },
    );

    engine.register_fn(
        "%register-key-scope",
        move |scope: SteelVal, label: String| -> steel::rvals::Result<()> {
            registry_for(id)?
                .borrow_mut()
                .plugins
                .keymap
                .register_scope(&name_string(scope)?, &label);
            Ok(())
        },
    );

    engine.register_fn(
        "map-key",
        move |scope: SteelVal,
              mode: SteelVal,
              keys: SteelVal,
              command: SteelVal|
              -> steel::rvals::Result<()> {
            registry_for(id)?.borrow_mut().plugins.keymap.map(
                &name_string(scope)?,
                &name_string(mode)?,
                key_sequence(keys)?,
                Some(name_string(command)?),
            );

            Ok(())
        },
    );

    engine.register_fn(
        "unmap-key",
        move |scope: SteelVal, mode: SteelVal, keys: SteelVal| -> steel::rvals::Result<()> {
            registry_for(id)?.borrow_mut().plugins.keymap.map(
                &name_string(scope)?,
                &name_string(mode)?,
                key_sequence(keys)?,
                None,
            );

            Ok(())
        },
    );

    engine.register_fn(
        "register-mode",
        move |kind: SteelVal, name: SteelVal| -> steel::rvals::Result<()> {
            registry_for(id)?
                .borrow_mut()
                .plugins
                .mode(&name_string(kind)?, &name_string(name)?);

            Ok(())
        },
    );

    engine.register_fn(
        "%register-context",
        move |kind: SteelVal, field: SteelVal, events: Vec<String>| -> steel::rvals::Result<()> {
            let kind = name_string(kind)?;
            let field = name_string(field)?;
            let registry = registry_for(id)?;
            let mut registry = registry.borrow_mut();
            registry.plugins.register_context(&kind, &field, events);

            Ok(())
        },
    );

    engine.register_fn(
        "host-call",
        move |operation: SteelVal, argument: SteelVal| -> steel::rvals::Result<SteelVal> {
            let operation = name_string(operation)?;
            let argument = Json::from_steelval(&argument)?.0;
            let registry = registry_for(id)?;
            let registry = registry.borrow();

            Json((registry.host)(operation, argument)).into_steelval()
        },
    );

    id
}

pub(super) fn remove(id: usize) {
    REGISTRIES.with_borrow_mut(|registries| registries.remove(&id));
}

fn registry_for(id: usize) -> steel::rvals::Result<Rc<RefCell<ScriptRegistry>>> {
    REGISTRIES
        .with_borrow(|registries| registries.get(&id).and_then(Weak::upgrade))
        .ok_or_else(|| {
            SteelErr::new(
                ErrorKind::Generic,
                "script runtime is no longer available".into(),
            )
        })
}

fn name_string(value: SteelVal) -> steel::rvals::Result<String> {
    match value {
        SteelVal::StringV(name) | SteelVal::SymbolV(name) => Ok(name.to_string()),
        _ => Err(SteelErr::new(
            ErrorKind::TypeMismatch,
            "expected string or symbol name".into(),
        )),
    }
}

fn key_sequence(value: SteelVal) -> steel::rvals::Result<Vec<String>> {
    let keys = match value {
        SteelVal::StringV(key) => vec![key.to_string()],
        value @ (SteelVal::ListV(_) | SteelVal::VectorV(_)) => {
            Vec::<SteelVal>::from_steelval(&value)?
                .into_iter()
                .map(name_string)
                .collect::<steel::rvals::Result<Vec<_>>>()?
        }
        _ => {
            return Err(SteelErr::new(
                ErrorKind::TypeMismatch,
                "keys must be a string, list, or vector of key tokens".into(),
            ));
        }
    };

    if keys.is_empty() || keys.iter().any(String::is_empty) {
        return Err(SteelErr::new(
            ErrorKind::Generic,
            "key sequences and their tokens must not be empty".into(),
        ));
    }

    Ok(keys)
}

fn unsigned_integer(value: &SteelVal) -> Option<u64> {
    match value {
        SteelVal::IntV(value) => u64::try_from(*value).ok(),
        SteelVal::BigNum(value) => u64::try_from(value.as_ref()).ok(),
        _ => None,
    }
}

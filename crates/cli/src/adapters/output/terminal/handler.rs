use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_input::backend::crossterm::EventHandler;

use super::app::App;

pub fn handle_key_event(app: &mut App, event: KeyEvent) {
    if app.input_mode {
        match event.code {
            KeyCode::Esc => {
                app.input_mode = false;
            }
            KeyCode::Enter => {
                app.input_mode = false;
            }
            _ => {
                app.query.handle_event(&crossterm::event::Event::Key(event));
                app.mark_query_changed();
                app.mark_user_interaction();
            }
        }
        return;
    }

    match (event.code, event.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => {
            app.should_quit = true;
        }
        (KeyCode::Char('/'), _) => {
            app.input_mode = true;
            app.mark_user_interaction();
        }
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
            app.move_down();
            app.mark_user_interaction();
        }
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
            app.move_up();
            app.mark_user_interaction();
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Enter, _) => {
            if app.can_accept_enter_selection() {
                app.accepted_scoped_id = app.selected_scoped_id();
                app.should_quit = true;
            } else {
                app.hint = Some(
                    "Selection is enabled after results are loaded and navigation keys are used."
                        .to_string(),
                );
            }
        }
        (KeyCode::Char('i'), _) => {
            app.mark_user_interaction();
            app.hint = app
                .selected_scoped_id()
                .map(|s| format!("Install: ato install {}", s));
        }
        (KeyCode::Char('m'), _) => {
            app.mark_user_interaction();
            app.show_manifest = !app.show_manifest;
            if !app.show_manifest {
                app.hint = Some("Manifest view: off".to_string());
            } else {
                app.hint = Some("Manifest view: on".to_string());
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{CapsuleSummary, PublisherInfo};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample_capsule() -> CapsuleSummary {
        CapsuleSummary {
            id: "01TEST".to_string(),
            slug: "demo-capsule".to_string(),
            scoped_id: Some("demo/demo-capsule".to_string()),
            name: "Demo Capsule".to_string(),
            description: "Demo".to_string(),
            category: "tools".to_string(),
            capsule_type: "app".to_string(),
            price: 0,
            currency: "usd".to_string(),
            publisher: PublisherInfo {
                handle: "demo".to_string(),
                author_did: "did:key:z6Mkhdemo".to_string(),
                verified: true,
            },
            latest_version: Some("1.0.0".to_string()),
            downloads: 0,
            created_at: "2026-03-01T00:00:00Z".to_string(),
            updated_at: "2026-03-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn enter_is_ignored_without_prior_user_interaction() {
        let mut app = App::new(None, false);
        app.items = vec![sample_capsule()];
        app.loading = false;

        handle_key_event(&mut app, key(KeyCode::Enter));

        assert!(!app.should_quit);
        assert!(app.accepted_scoped_id.is_none());
    }

    #[test]
    fn enter_selects_after_navigation_interaction() {
        let mut app = App::new(None, false);
        app.items = vec![sample_capsule()];
        app.loading = false;

        handle_key_event(&mut app, key(KeyCode::Down));
        handle_key_event(&mut app, key(KeyCode::Enter));

        assert!(app.should_quit);
        assert_eq!(app.accepted_scoped_id.as_deref(), Some("demo/demo-capsule"));
    }
}

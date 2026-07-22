mod icon_theme_selector;

use fs::Fs;
use fuzzy::{StringMatch, StringMatchCandidate, match_strings};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, Focusable, Render, UpdateGlobal, WeakEntity,
    Window, actions,
};
use picker::{Picker, PickerDelegate};
use settings::{Settings, SettingsStore, update_settings_file};
use std::sync::Arc;
use theme::{
    Appearance, ConfiguredTheme, SystemAppearance, Theme, ThemeMeta, ThemeRegistry, WindowTheme,
};
use theme_settings::{
    ThemeAppearanceMode, ThemeName, ThemeSelection, ThemeSettings, appearance_to_mode,
};
use ui::{ListItem, ListItemSpacing, prelude::*, v_flex};
use util::ResultExt;
use workspace::{ModalView, Workspace, ui::HighlightedLabel, with_active_or_new_workspace};
use zed_actions::{ExtensionCategoryFilter, Extensions};

use crate::icon_theme_selector::{IconThemeSelector, IconThemeSelectorDelegate};

actions!(
    theme_selector,
    [
        /// Reloads all themes from disk.
        Reload
    ]
);

pub fn init(cx: &mut App) {
    cx.on_action(|action: &zed_actions::theme_selector::Toggle, cx| {
        let action = action.clone();
        with_active_or_new_workspace(cx, move |workspace, window, cx| {
            toggle_theme_selector(
                workspace,
                action.themes_filter.as_ref(),
                ThemeSelectorScope::Global,
                window,
                cx,
            );
        });
    });
    cx.on_action(
        |action: &zed_actions::theme_selector::ToggleWindowTheme, cx| {
            let action = action.clone();
            with_active_or_new_workspace(cx, move |workspace, window, cx| {
                toggle_theme_selector(
                    workspace,
                    action.themes_filter.as_ref(),
                    ThemeSelectorScope::Window,
                    window,
                    cx,
                );
            });
        },
    );
    cx.on_action(|_: &zed_actions::theme_selector::ClearWindowTheme, cx| {
        with_active_or_new_workspace(cx, move |workspace, window, cx| {
            workspace.clear_window_theme(window, cx);
        });
    });
    cx.on_action(|action: &zed_actions::icon_theme_selector::Toggle, cx| {
        let action = action.clone();
        with_active_or_new_workspace(cx, move |workspace, window, cx| {
            toggle_icon_theme_selector(workspace, &action, window, cx);
        });
    });
}

fn toggle_theme_selector(
    workspace: &mut Workspace,
    themes_filter: Option<&Vec<String>>,
    scope: ThemeSelectorScope,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let fs = workspace.app_state().fs.clone();
    workspace.toggle_modal(window, cx, |window, cx| {
        let delegate = ThemeSelectorDelegate::new(
            cx.entity().downgrade(),
            fs,
            themes_filter,
            scope,
            window,
            cx,
        );
        ThemeSelector::new(delegate, window, cx)
    });
}

fn toggle_icon_theme_selector(
    workspace: &mut Workspace,
    toggle: &zed_actions::icon_theme_selector::Toggle,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let fs = workspace.app_state().fs.clone();
    workspace.toggle_modal(window, cx, |window, cx| {
        let delegate = IconThemeSelectorDelegate::new(
            cx.entity().downgrade(),
            fs,
            toggle.themes_filter.as_ref(),
            cx,
        );
        IconThemeSelector::new(delegate, window, cx)
    });
}

impl ModalView for ThemeSelector {
    fn on_before_dismiss(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> workspace::DismissDecision {
        self.picker.update(cx, |picker, cx| {
            picker.delegate.revert_theme(window, cx);
        });
        workspace::DismissDecision::Dismiss(true)
    }
}

struct ThemeSelector {
    picker: Entity<Picker<ThemeSelectorDelegate>>,
}

impl EventEmitter<DismissEvent> for ThemeSelector {}

impl Focusable for ThemeSelector {
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for ThemeSelector {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("ThemeSelector")
            .w(rems(34.))
            .child(self.picker.clone())
    }
}

impl ThemeSelector {
    pub fn new(
        delegate: ThemeSelectorDelegate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThemeSelectorScope {
    Global,
    Window,
}

struct ThemeSelectorDelegate {
    fs: Arc<dyn Fs>,
    themes: Vec<ThemeMeta>,
    matches: Vec<StringMatch>,
    /// The theme that was selected before the `ThemeSelector` menu was opened.
    ///
    /// We use this to return back to theme that was set if the user dismisses the menu.
    original_theme_settings: ThemeSettings,
    /// The current system appearance.
    original_system_appearance: Appearance,
    /// Window-scope: theme that was active for this window before the picker opened.
    original_window_theme: Arc<Theme>,
    /// Window-scope: whether the window already had an override before the picker opened.
    original_had_window_override: bool,
    /// Whether this picker writes a window override or global settings.
    scope: ThemeSelectorScope,
    /// The id of the original theme in the list of themes.
    /// Using `Option<usize>` instead of `usize` because it's possible that the
    /// original theme is not present in the list of themes when it is first
    /// built, depending on the provided `themes_filter`. For example, when a
    /// theme is installed, the `themes_filter` is set to the new theme names
    /// and, if we used `unwrap_or(0)` as a fallback, the first theme in the
    /// list would be shown as "active".
    original_theme_id: Option<usize>,
    /// The currently selected new theme.
    new_theme: Arc<Theme>,
    selection_completed: bool,
    selected_theme: Option<Arc<Theme>>,
    selected_index: usize,
    selector: WeakEntity<ThemeSelector>,
}

impl ThemeSelectorDelegate {
    fn new(
        selector: WeakEntity<ThemeSelector>,
        fs: Arc<dyn Fs>,
        themes_filter: Option<&Vec<String>>,
        scope: ThemeSelectorScope,
        window: &Window,
        cx: &mut Context<ThemeSelector>,
    ) -> Self {
        let original_window_theme = window.theme(cx).clone();
        let original_had_window_override =
            theme::WindowThemeOverrides::override_for(window.window_handle().window_id(), cx)
                .is_some();
        let original_theme = match scope {
            // The global selector edits `settings.json`, so it must be seeded
            // with the configured theme rather than this window's effective
            // theme; otherwise, in a window that has a per-window override,
            // confirming without navigating would write the override theme
            // into the global settings.
            ThemeSelectorScope::Global => cx.configured_theme().clone(),
            ThemeSelectorScope::Window => original_window_theme.clone(),
        };
        let original_theme_settings = ThemeSettings::get_global(cx).clone();
        let original_system_appearance = SystemAppearance::global(cx).0;

        let registry = ThemeRegistry::global(cx);
        let mut themes = registry
            .list()
            .into_iter()
            .filter(|meta| {
                if let Some(theme_filter) = themes_filter {
                    theme_filter.contains(&meta.name.to_string())
                } else {
                    true
                }
            })
            .collect::<Vec<_>>();

        // Sort by dark vs light, then by name.
        themes.sort_unstable_by(|a, b| {
            a.appearance
                .is_light()
                .cmp(&b.appearance.is_light())
                .then(a.name.cmp(&b.name))
        });

        let original_theme_id = themes
            .iter()
            .position(|meta| meta.name == original_theme.name);

        let matches: Vec<StringMatch> = themes
            .iter()
            .enumerate()
            .map(|(id, meta)| StringMatch {
                candidate_id: id,
                score: 0.0,
                positions: Default::default(),
                string: meta.name.to_string(),
            })
            .collect();

        // The current theme is likely in this list, so default to first showing that.
        let selected_index = matches
            .iter()
            .position(|mat| mat.string == original_theme.name)
            .unwrap_or(0);

        Self {
            fs,
            themes,
            matches,
            original_theme_settings,
            original_system_appearance,
            original_window_theme,
            original_had_window_override,
            scope,
            original_theme_id,
            new_theme: original_theme, // Start with the original theme.
            selected_index,
            selection_completed: false,
            selected_theme: None,
            selector,
        }
    }

    fn is_original_theme(&self, index: usize) -> bool {
        self.matches
            .get(index)
            .zip(self.original_theme_id)
            .is_some_and(|(mat, original_theme_id)| mat.candidate_id == original_theme_id)
    }

    fn show_selected_theme(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Picker<ThemeSelectorDelegate>>,
    ) -> Option<Arc<Theme>> {
        if let Some(mat) = self.matches.get(self.selected_index) {
            let registry = ThemeRegistry::global(cx);

            match registry.get(&mat.string) {
                Ok(theme) => {
                    self.set_theme(theme.clone(), window, cx);
                    Some(theme)
                }
                Err(error) => {
                    log::error!("error loading theme {}: {}", mat.string, error);
                    None
                }
            }
        } else {
            None
        }
    }

    fn revert_theme(&mut self, window: &mut Window, cx: &mut App) {
        if self.selection_completed {
            return;
        }
        match self.scope {
            ThemeSelectorScope::Global => {
                SettingsStore::update_global(cx, |store, _| {
                    store.override_global(self.original_theme_settings.clone());
                });
                self.restore_window_override(window, cx);
            }
            ThemeSelectorScope::Window => {
                if self.original_had_window_override {
                    theme::WindowThemeOverrides::apply_to_window(
                        window,
                        self.original_window_theme.clone(),
                        cx,
                    );
                } else {
                    theme::WindowThemeOverrides::clear_for_window(window, cx);
                }
            }
        }
        self.selection_completed = true;
    }

    /// Global-scope only: puts back this window's own override after a preview
    /// temporarily painted over it.
    fn restore_window_override(&self, window: &mut Window, cx: &mut App) {
        if self.original_had_window_override {
            theme::WindowThemeOverrides::apply_to_window(
                window,
                self.original_window_theme.clone(),
                cx,
            );
        }
    }

    fn set_theme(&mut self, new_theme: Arc<Theme>, window: &mut Window, cx: &mut App) {
        match self.scope {
            ThemeSelectorScope::Global => {
                // Update the global (in-memory) theme settings.
                SettingsStore::update_global(cx, |store, _| {
                    override_global_theme(
                        store,
                        &new_theme,
                        &self.original_theme_settings.theme,
                        self.original_system_appearance,
                    )
                });
                // A window override would mask the preview, so temporarily
                // paint the previewed theme over it; it is restored on
                // confirm or dismiss.
                if self.original_had_window_override {
                    theme::WindowThemeOverrides::apply_to_window(window, new_theme.clone(), cx);
                }
            }
            ThemeSelectorScope::Window => {
                theme::WindowThemeOverrides::apply_to_window(window, new_theme.clone(), cx);
            }
        }

        self.new_theme = new_theme;
    }
}

/// Overrides the global (in-memory) theme settings.
///
/// Note that this does **not** update the user's `settings.json` file (see the
/// [`ThemeSelectorDelegate::confirm`] method and [`theme_settings::set_theme`] function).
fn override_global_theme(
    store: &mut SettingsStore,
    new_theme: &Theme,
    original_theme: &ThemeSelection,
    system_appearance: Appearance,
) {
    let theme_name = ThemeName(new_theme.name.clone().into());
    let new_appearance = new_theme.appearance();
    let new_theme_is_light = new_appearance.is_light();

    let mut curr_theme_settings = store.get::<ThemeSettings>(None).clone();

    match (original_theme, &curr_theme_settings.theme) {
        // Override the currently selected static theme.
        (ThemeSelection::Static(_), ThemeSelection::Static(_)) => {
            curr_theme_settings.theme = ThemeSelection::Static(theme_name);
        }

        // If the current theme selection is dynamic, then only override the global setting for the
        // specific mode (light or dark).
        (
            ThemeSelection::Dynamic {
                mode: original_mode,
                light: original_light,
                dark: original_dark,
            },
            ThemeSelection::Dynamic { .. },
        ) => {
            let new_mode = update_mode_if_new_appearance_is_different_from_system(
                original_mode,
                system_appearance,
                new_appearance,
            );

            let updated_theme = retain_original_opposing_theme(
                new_theme_is_light,
                new_mode,
                theme_name,
                original_light,
                original_dark,
            );

            curr_theme_settings.theme = updated_theme;
        }

        // The theme selection mode changed while selecting new themes (someone edited the settings
        // file on disk while we had the dialogue open), so don't do anything.
        _ => return,
    };

    store.override_global(curr_theme_settings);
}

/// Helper function for determining the new [`ThemeAppearanceMode`] for the new theme.
///
/// If the the original theme mode was [`System`] and the new theme's appearance matches the system
/// appearance, we don't need to change the mode setting.
///
/// Otherwise, we need to change the mode in order to see the new theme.
///
/// [`System`]: ThemeAppearanceMode::System
fn update_mode_if_new_appearance_is_different_from_system(
    original_mode: &ThemeAppearanceMode,
    system_appearance: Appearance,
    new_appearance: Appearance,
) -> ThemeAppearanceMode {
    if original_mode == &ThemeAppearanceMode::System && system_appearance == new_appearance {
        ThemeAppearanceMode::System
    } else {
        appearance_to_mode(new_appearance)
    }
}

/// Helper function for updating / displaying the [`ThemeSelection`] while using the theme selector.
///
/// We want to retain the alternate theme selection of the original settings (before the menu was
/// opened), not the currently selected theme (which likely has changed multiple times while the
/// menu has been open).
fn retain_original_opposing_theme(
    new_theme_is_light: bool,
    new_mode: ThemeAppearanceMode,
    theme_name: ThemeName,
    original_light: &ThemeName,
    original_dark: &ThemeName,
) -> ThemeSelection {
    if new_theme_is_light {
        ThemeSelection::Dynamic {
            mode: new_mode,
            light: theme_name,
            dark: original_dark.clone(),
        }
    } else {
        ThemeSelection::Dynamic {
            mode: new_mode,
            light: original_light.clone(),
            dark: theme_name,
        }
    }
}

impl PickerDelegate for ThemeSelectorDelegate {
    type ListItem = ui::ListItem;

    fn name() -> &'static str {
        "theme selector"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        match self.scope {
            ThemeSelectorScope::Global => "Select Theme...".into(),
            ThemeSelectorScope::Window => "Select Window Theme...".into(),
        }
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn confirm(
        &mut self,
        _secondary: bool,
        window: &mut Window,
        cx: &mut Context<Picker<ThemeSelectorDelegate>>,
    ) {
        self.selection_completed = true;

        let theme = self.new_theme.clone();
        let theme_name: Arc<str> = theme.name.as_str().into();

        match self.scope {
            ThemeSelectorScope::Global => {
                let theme_appearance = theme.appearance;
                let system_appearance = SystemAppearance::global(cx).0;

                telemetry::event!("Settings Changed", setting = "theme", value = theme_name);

                update_settings_file(self.fs.clone(), cx, move |settings, _| {
                    theme_settings::set_theme(
                        settings,
                        theme_name,
                        theme_appearance,
                        system_appearance,
                    );
                });
                // This window's own override still wins over the new
                // configured theme.
                self.restore_window_override(window, cx);
            }
            ThemeSelectorScope::Window => {
                telemetry::event!("Window Theme Changed", value = theme_name);
                if let Some(workspace) = Workspace::for_window(window, cx) {
                    workspace.update(cx, |workspace, cx| {
                        workspace.set_window_theme(theme, window, cx);
                    });
                } else {
                    theme::WindowThemeOverrides::apply_to_window(window, theme, cx);
                }
            }
        }

        self.selector
            .update(cx, |_, cx| {
                cx.emit(DismissEvent);
            })
            .ok();
    }

    fn dismissed(&mut self, window: &mut Window, cx: &mut Context<Picker<ThemeSelectorDelegate>>) {
        self.revert_theme(window, cx);

        self.selector
            .update(cx, |_, cx| cx.emit(DismissEvent))
            .log_err();
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Picker<ThemeSelectorDelegate>>,
    ) {
        self.selected_index = ix;
        self.selected_theme = self.show_selected_theme(window, cx);
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<ThemeSelectorDelegate>>,
    ) -> gpui::Task<()> {
        let background = cx.background_executor().clone();
        let candidates = self
            .themes
            .iter()
            .enumerate()
            .map(|(id, meta)| StringMatchCandidate::new(id, &meta.name))
            .collect::<Vec<_>>();

        cx.spawn_in(window, async move |this, cx| {
            let matches = if query.is_empty() {
                candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| StringMatch {
                        candidate_id: index,
                        string: candidate.string,
                        positions: Vec::new(),
                        score: 0.0,
                    })
                    .collect()
            } else {
                match_strings(
                    &candidates,
                    &query,
                    false,
                    true,
                    100,
                    &Default::default(),
                    background,
                )
                .await
            };

            this.update_in(cx, |this, window, cx| {
                this.delegate.matches = matches;
                if query.is_empty() && this.delegate.selected_theme.is_none() {
                    this.delegate.selected_index = this
                        .delegate
                        .selected_index
                        .min(this.delegate.matches.len().saturating_sub(1));
                } else if let Some(selected) = this.delegate.selected_theme.as_ref() {
                    this.delegate.selected_index = this
                        .delegate
                        .matches
                        .iter()
                        .enumerate()
                        .find(|(_, mtch)| mtch.string == selected.name)
                        .map(|(ix, _)| ix)
                        .unwrap_or_default();
                } else {
                    this.delegate.selected_index = 0;
                }
                // Preserve the previously selected theme when the filter yields no results.
                if let Some(theme) = this.delegate.show_selected_theme(window, cx) {
                    this.delegate.selected_theme = Some(theme);
                }
            })
            .log_err();
        })
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let theme_match = &self.matches.get(ix)?;
        let is_original_theme = self.is_original_theme(ix);

        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(HighlightedLabel::new(
                    theme_match.string.clone(),
                    theme_match.positions.clone(),
                ))
                .when(is_original_theme, |this| {
                    this.end_slot(Icon::new(IconName::Check).color(Color::Muted))
                }),
        )
    }

    fn render_footer(
        &self,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<gpui::AnyElement> {
        Some(
            h_flex()
                .p_2()
                .w_full()
                .justify_between()
                .gap_2()
                .border_t_1()
                .border_color(window.theme(cx).colors().border_variant)
                .child(
                    Button::new("docs", "View Theme Docs")
                        .end_icon(
                            Icon::new(IconName::ArrowUpRight)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        )
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.open_url("https://zed.dev/docs/themes");
                        })),
                )
                .child(
                    Button::new("more-themes", "Install Themes").on_click(cx.listener({
                        move |_, _, window, cx| {
                            window.dispatch_action(
                                Box::new(Extensions {
                                    category_filter: Some(ExtensionCategoryFilter::Themes),
                                    id: None,
                                }),
                                cx,
                            );
                        }
                    })),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use project::Project;
    use serde_json::json;
    use theme::{Appearance, ThemeFamily, ThemeRegistry, default_color_scales};
    use util::path;
    use workspace::MultiWorkspace;

    fn init_test(cx: &mut TestAppContext) -> Arc<workspace::AppState> {
        cx.update(|cx| {
            let app_state = workspace::AppState::test(cx);
            settings::init(cx);
            theme::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            super::init(cx);
            app_state
        })
    }

    fn register_test_themes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let registry = ThemeRegistry::global(cx);
            let base_theme = registry.get("One Dark").unwrap();

            let mut test_light = (*base_theme).clone();
            test_light.id = "test-light".to_string();
            test_light.name = "Test Light".into();
            test_light.appearance = Appearance::Light;

            let mut test_dark_a = (*base_theme).clone();
            test_dark_a.id = "test-dark-a".to_string();
            test_dark_a.name = "Test Dark A".into();

            let mut test_dark_b = (*base_theme).clone();
            test_dark_b.id = "test-dark-b".to_string();
            test_dark_b.name = "Test Dark B".into();

            registry.register_test_themes([ThemeFamily {
                id: "test-family".to_string(),
                name: "Test Family".into(),
                author: "test".into(),
                themes: vec![test_light, test_dark_a, test_dark_b],
                scales: default_color_scales(),
            }]);
        });
    }

    async fn setup_test(cx: &mut TestAppContext) -> Arc<workspace::AppState> {
        let app_state = init_test(cx);
        register_test_themes(cx);
        app_state
            .fs
            .as_fake()
            .insert_tree(path!("/test"), json!({}))
            .await;
        app_state
    }

    fn open_theme_selector(
        workspace: &Entity<workspace::Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<Picker<ThemeSelectorDelegate>> {
        cx.dispatch_action(zed_actions::theme_selector::Toggle {
            themes_filter: None,
        });
        cx.run_until_parked();
        workspace.update(cx, |workspace, cx| {
            workspace
                .active_modal::<ThemeSelector>(cx)
                .expect("theme selector should be open")
                .read(cx)
                .picker
                .clone()
        })
    }

    fn selected_theme_name(
        picker: &Entity<Picker<ThemeSelectorDelegate>>,
        cx: &mut VisualTestContext,
    ) -> String {
        picker.read_with(cx, |picker, _| {
            picker
                .delegate
                .matches
                .get(picker.delegate.selected_index)
                .expect("selected index should point to a match")
                .string
                .clone()
        })
    }

    fn previewed_theme_name(
        picker: &Entity<Picker<ThemeSelectorDelegate>>,
        cx: &mut VisualTestContext,
    ) -> String {
        picker.read_with(cx, |picker, _| picker.delegate.new_theme.name.to_string())
    }

    fn open_window_theme_selector(
        workspace: &Entity<workspace::Workspace>,
        cx: &mut VisualTestContext,
    ) -> Entity<Picker<ThemeSelectorDelegate>> {
        cx.dispatch_action(zed_actions::theme_selector::ToggleWindowTheme {
            themes_filter: None,
        });
        cx.run_until_parked();
        workspace.update(cx, |workspace, cx| {
            workspace
                .active_modal::<ThemeSelector>(cx)
                .expect("theme selector should be open")
                .read(cx)
                .picker
                .clone()
        })
    }

    fn select_and_confirm(
        picker: &Entity<Picker<ThemeSelectorDelegate>>,
        theme_name: &str,
        cx: &mut VisualTestContext,
    ) {
        let target_index = picker.read_with(cx, |picker, _| {
            picker
                .delegate
                .matches
                .iter()
                .position(|m| m.string == theme_name)
                .expect("theme should be in the match list")
        });
        picker.update_in(cx, |picker, window, cx| {
            picker.set_selected_index(target_index, None, true, window, cx);
        });
        cx.run_until_parked();
        picker.update_in(cx, |picker, window, cx| {
            picker.delegate.confirm(false, window, cx);
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn test_window_theme_selection_and_clear(cx: &mut TestAppContext) {
        let app_state = setup_test(cx).await;
        let project = Project::test(app_state.fs.clone(), [path!("/test").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());

        let global_theme = cx.update(|_, cx| cx.configured_theme().name.to_string());

        let picker = open_window_theme_selector(&workspace, cx);
        select_and_confirm(&picker, "Test Dark A", cx);

        let (window_theme, global_theme_now) = cx.update(|window, cx| {
            (
                window.theme(cx).name.to_string(),
                cx.configured_theme().name.to_string(),
            )
        });
        assert_eq!(
            window_theme, "Test Dark A",
            "confirming a window-scoped selection should override the window theme"
        );
        assert_eq!(
            global_theme_now, global_theme,
            "a window-scoped selection must not change the configured theme"
        );
        workspace.read_with(cx, |workspace, _| {
            assert_eq!(
                workspace.theme_override().map(|name| name.to_string()),
                Some("Test Dark A".to_string())
            );
        });

        cx.dispatch_action(zed_actions::theme_selector::ClearWindowTheme);
        cx.run_until_parked();

        let window_theme = cx.update(|window, cx| window.theme(cx).name.to_string());
        assert_eq!(
            window_theme, global_theme,
            "clearing the override should restore the configured theme"
        );
        workspace.read_with(cx, |workspace, _| {
            assert!(workspace.theme_override().is_none());
        });
    }

    #[gpui::test]
    async fn test_window_theme_dismiss_reverts_preview(cx: &mut TestAppContext) {
        let app_state = setup_test(cx).await;
        let project = Project::test(app_state.fs.clone(), [path!("/test").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());

        let original_theme = cx.update(|window, cx| window.theme(cx).name.to_string());

        let picker = open_window_theme_selector(&workspace, cx);
        let target_index = picker.read_with(cx, |picker, _| {
            picker
                .delegate
                .matches
                .iter()
                .position(|m| m.string == "Test Dark B")
                .expect("theme should be in the match list")
        });
        picker.update_in(cx, |picker, window, cx| {
            picker.set_selected_index(target_index, None, true, window, cx);
        });
        cx.run_until_parked();

        assert_eq!(
            cx.update(|window, cx| window.theme(cx).name.to_string()),
            "Test Dark B",
            "selection should preview in the window"
        );

        picker.update_in(cx, |picker, window, cx| {
            picker.delegate.dismissed(window, cx);
        });
        cx.run_until_parked();

        assert_eq!(
            cx.update(|window, cx| window.theme(cx).name.to_string()),
            original_theme,
            "dismissing without confirming should revert the window theme"
        );
    }

    #[gpui::test]
    async fn test_theme_selector_preserves_selection_on_empty_filter(cx: &mut TestAppContext) {
        let app_state = setup_test(cx).await;
        let project = Project::test(app_state.fs.clone(), [path!("/test").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project.clone(), window, cx));
        let workspace =
            multi_workspace.read_with(cx, |multi_workspace, _| multi_workspace.workspace().clone());
        let picker = open_theme_selector(&workspace, cx);

        let target_index = picker.read_with(cx, |picker, _| {
            picker
                .delegate
                .matches
                .iter()
                .position(|m| m.string == "Test Light")
                .unwrap()
        });
        picker.update_in(cx, |picker, window, cx| {
            picker.set_selected_index(target_index, None, true, window, cx);
        });
        cx.run_until_parked();

        assert_eq!(previewed_theme_name(&picker, cx), "Test Light");

        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches("zzz".to_string(), window, cx);
        });
        cx.run_until_parked();

        picker.update_in(cx, |picker, window, cx| {
            picker.update_matches("".to_string(), window, cx);
        });
        cx.run_until_parked();

        assert_eq!(
            selected_theme_name(&picker, cx),
            "Test Light",
            "selected theme should be preserved after clearing an empty filter"
        );
        assert_eq!(
            previewed_theme_name(&picker, cx),
            "Test Light",
            "previewed theme should be preserved after clearing an empty filter"
        );
    }
}

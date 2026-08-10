use crate::prelude::*;
use gpui::WindowBackgroundAppearance;

/// Returns the [WindowBackgroundAppearance].
fn window_appearance(theme: &impl ActiveTheme) -> WindowBackgroundAppearance {
    theme.theme().styles.window_background_appearance
}

/// Returns if the window and it's surfaces are expected
/// to be transparent.
///
/// Helps determine if you need to take extra steps to prevent
/// transparent backgrounds.
pub fn theme_is_transparent(theme: &impl ActiveTheme) -> bool {
    matches!(
        window_appearance(theme),
        WindowBackgroundAppearance::Transparent | WindowBackgroundAppearance::Blurred
    )
}

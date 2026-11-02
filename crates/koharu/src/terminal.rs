use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_EXTENDED_FLAGS, ENABLE_QUICK_EDIT_MODE, GetConsoleMode, GetStdHandle,
    STD_INPUT_HANDLE, SetConsoleMode,
};

/// Keep the desktop's diagnostic console from suspending work when clicked.
pub fn prevent_selection_pause() {
    // SAFETY: These APIs use the existing standard input handle without taking ownership.
    // Redirected input or a launch without a console is intentionally left alone.
    unsafe {
        let Ok(input) = GetStdHandle(STD_INPUT_HANDLE) else {
            return;
        };
        let mut mode = CONSOLE_MODE::default();
        if GetConsoleMode(input, &mut mode).is_err() {
            return;
        }
        // EXTENDED_FLAGS is required for Windows to apply a Quick Edit change.
        let mode = (mode | ENABLE_EXTENDED_FLAGS) & !ENABLE_QUICK_EDIT_MODE;
        let _ = SetConsoleMode(input, mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Console::GetConsoleProcessList;

    #[test]
    #[ignore = "Run this test binary in a hidden, dedicated Windows console"]
    fn startup_disables_quick_edit_and_preserves_other_console_input_modes()
    -> windows::core::Result<()> {
        // Run in a hidden, dedicated console, never the user's application console.
        unsafe {
            let mut processes = [0; 2];
            assert_eq!(
                GetConsoleProcessList(&mut processes),
                1,
                "use an isolated console"
            );
            let input = GetStdHandle(STD_INPUT_HANDLE)?;
            let mut original = CONSOLE_MODE::default();
            GetConsoleMode(input, &mut original)?;
            let enabled = original | ENABLE_EXTENDED_FLAGS | ENABLE_QUICK_EDIT_MODE;
            SetConsoleMode(input, enabled)?;
            prevent_selection_pause();
            let mut after = CONSOLE_MODE::default();
            GetConsoleMode(input, &mut after)?;
            assert!(
                !after.contains(ENABLE_QUICK_EDIT_MODE),
                "mouse selection must not pause desktop work"
            );
            assert!(after.contains(ENABLE_EXTENDED_FLAGS));
            assert_eq!(
                after & !ENABLE_QUICK_EDIT_MODE,
                enabled & !ENABLE_QUICK_EDIT_MODE
            );
            prevent_selection_pause();
            let mut repeated = CONSOLE_MODE::default();
            GetConsoleMode(input, &mut repeated)?;
            assert_eq!(repeated, after);
        }
        Ok(())
    }
}

#[cfg(not(windows))]
pub fn pick_file_or_folder() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new().pick_file()
}

#[cfg(windows)]
pub use windows_dialog::pick_file_or_folder;

#[cfg(windows)]
mod windows_dialog {
    use std::{cell::Cell, ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
    use windows::{
        Win32::{
            Foundation::{ERROR_CANCELLED, S_OK},
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
            },
            UI::Shell::{
                FDE_OVERWRITE_RESPONSE, FDE_SHAREVIOLATION_RESPONSE, FDEOR_DEFAULT, FDESVR_DEFAULT,
                FOS_FORCEFILESYSTEM, FileOpenDialog, IFileDialog, IFileDialogControlEvents,
                IFileDialogControlEvents_Impl, IFileDialogCustomize, IFileDialogEvents,
                IFileDialogEvents_Impl, IFileOpenDialog, IShellItem, SIGDN_FILESYSPATH,
            },
        },
        core::{BOOL, ComObject, HRESULT, Interface, Ref, Result, implement, w},
    };

    const SELECT_FOLDER_BUTTON: u32 = 1;

    pub fn pick_file_or_folder() -> Option<PathBuf> {
        match show_dialog() {
            Ok(picked) => picked,
            Err(err) => {
                log::error!("File picker failed: {err}");
                None
            }
        }
    }

    struct ComApartment;

    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    fn show_dialog() -> Result<Option<PathBuf>> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }.ok()?;
        let _apartment = ComApartment;
        let dialog: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)? };
        unsafe { dialog.SetOptions(dialog.GetOptions()? | FOS_FORCEFILESYSTEM)? };
        let customize: IFileDialogCustomize = dialog.cast()?;
        unsafe {
            customize.AddPushButton(SELECT_FOLDER_BUTTON, w!("Select folder"))?;
            customize.MakeProminent(SELECT_FOLDER_BUTTON)?;
        }
        let events = ComObject::new(DialogEvents::default());
        unsafe { dialog.Advise(&events.to_interface::<IFileDialogEvents>())? };

        let shown = unsafe { dialog.Show(None) };
        if let Some(folder) = events.picked_folder.take() {
            return Ok(Some(folder));
        }
        match shown {
            Ok(()) => file_system_path(&unsafe { dialog.GetResult()? }).map(Some),
            Err(err) if err.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn file_system_path(item: &IShellItem) -> Result<PathBuf> {
        let name = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH)? };
        let path = PathBuf::from(OsString::from_wide(unsafe { name.as_wide() }));
        unsafe { CoTaskMemFree(Some(name.0.cast_const().cast())) };
        Ok(path)
    }

    fn folder_to_pick(dialog: &IFileDialog) -> Result<PathBuf> {
        let highlighted = unsafe { dialog.GetCurrentSelection() }
            .and_then(|item| file_system_path(&item))
            .ok()
            .filter(|path| path.is_dir());
        match highlighted {
            Some(folder) => Ok(folder),
            None => file_system_path(&unsafe { dialog.GetFolder()? }),
        }
    }

    #[implement(IFileDialogEvents, IFileDialogControlEvents)]
    #[derive(Default)]
    struct DialogEvents {
        picked_folder: Cell<Option<PathBuf>>,
    }

    impl IFileDialogControlEvents_Impl for DialogEvents_Impl {
        fn OnButtonClicked(
            &self,
            customize: Ref<IFileDialogCustomize>,
            control: u32,
        ) -> Result<()> {
            assert_eq!(control, SELECT_FOLDER_BUTTON);
            let dialog: IFileDialog = customize.ok()?.cast()?;
            match folder_to_pick(&dialog) {
                Ok(folder) => {
                    self.picked_folder.set(Some(folder));
                    unsafe { dialog.Close(S_OK) }
                }
                Err(err) => {
                    log::warn!("Select folder: not a file system folder: {err}");
                    Ok(())
                }
            }
        }

        fn OnItemSelected(&self, _: Ref<IFileDialogCustomize>, _: u32, _: u32) -> Result<()> {
            Ok(())
        }

        fn OnCheckButtonToggled(
            &self,
            _: Ref<IFileDialogCustomize>,
            _: u32,
            _: BOOL,
        ) -> Result<()> {
            Ok(())
        }

        fn OnControlActivating(&self, _: Ref<IFileDialogCustomize>, _: u32) -> Result<()> {
            Ok(())
        }
    }

    impl IFileDialogEvents_Impl for DialogEvents_Impl {
        fn OnFileOk(&self, _: Ref<IFileDialog>) -> Result<()> {
            Ok(())
        }

        fn OnFolderChanging(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> Result<()> {
            Ok(())
        }

        fn OnFolderChange(&self, _: Ref<IFileDialog>) -> Result<()> {
            Ok(())
        }

        fn OnSelectionChange(&self, _: Ref<IFileDialog>) -> Result<()> {
            Ok(())
        }

        fn OnShareViolation(
            &self,
            _: Ref<IFileDialog>,
            _: Ref<IShellItem>,
        ) -> Result<FDE_SHAREVIOLATION_RESPONSE> {
            Ok(FDESVR_DEFAULT)
        }

        fn OnTypeChange(&self, _: Ref<IFileDialog>) -> Result<()> {
            Ok(())
        }

        fn OnOverwrite(
            &self,
            _: Ref<IFileDialog>,
            _: Ref<IShellItem>,
        ) -> Result<FDE_OVERWRITE_RESPONSE> {
            Ok(FDEOR_DEFAULT)
        }
    }
}

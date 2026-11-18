use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Folder1,
    Folder2,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Side::Folder1 => write!(f, "Left"),
            Side::Folder2 => write!(f, "Right"),
        }
    }
}

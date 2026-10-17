//! Colorimetry of Y'CbCr video frames, shared by live view and review.

/// How Y'CbCr values map to R'G'B'.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YuvColorimetry {
    pub matrix: YuvMatrix,
    /// Full range (0–255) instead of limited range (Y 16–235, CbCr 16–240).
    pub full_range: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

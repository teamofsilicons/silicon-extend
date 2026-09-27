//! Helpers for declaring wire types.

/// Declares a wire enum that is open to values this build doesn't know: they decode as `Other`
/// instead of failing, so a newer service or app can add a value without breaking older readers.
/// (The 1.0 enums, `ErrorCode`, `EndReason`, `Capability`, `Visibility` and `DeviceOs`, are closed:
/// a 1.0 reader refuses a value it doesn't know, so they never gain one.)
macro_rules! open_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        pub enum $name {
            $( $(#[$vmeta])* #[serde(rename = $wire)] $variant, )+
            /// A value this build doesn't know, from a newer service or app. Written as `"other"`.
            #[serde(other, rename = "other")]
            Other,
        }

        impl $name {
            /// Every known value, in declaration order (without `Other`).
            pub const ALL: &'static [Self] = &[$( Self::$variant, )+];

            /// The wire value.
            pub fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $wire, )+
                    Self::Other => "other",
                }
            }

            /// Reads a wire value (a database column, a query parameter); unknown values read as `Other`.
            pub fn parse(s: &str) -> Self {
                match s {
                    $( $wire => Self::$variant, )+
                    _ => Self::Other,
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

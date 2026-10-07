#![crate_type = "lib"]

pub struct Name(pub String);

impl Name {
    pub fn as_str(&self) -> &str { &self.0 }
}

pub unsafe fn name_unchecked(name: String) -> Name { Name(name) }

pub struct Len<const N: usize>;

impl<const N: usize> Len<N> {
    pub fn get(&self) -> usize { N }
}

// NOTE: The value of each block borrows a temporary of its tail expression.
//       Before edition 2024, that temporary lives until the end of the enclosing statement.
#[macro_export]
macro_rules! name_len {
    ($name:expr) => {
        { let name = $name; $crate::Name(name).as_str() }.len()
    };
}

#[macro_export]
macro_rules! unsafe_name_len {
    ($name:expr) => {
        unsafe { $crate::name_unchecked($name).as_str() }.len()
    };
}

#[macro_export]
macro_rules! labeled_name_len {
    ($name:expr) => {
        ('name: { if $name.is_empty() { break 'name ""; } $crate::Name($name).as_str() }).len()
    };
}

// NOTE: A closure body and a const generic argument do not accept a macro call in place of the block.
#[macro_export]
macro_rules! closure_name_len {
    ($name:expr) => {
        (|| -> usize { $crate::Name($name).as_str().len() })()
    };
}

#[macro_export]
macro_rules! const_arg_len {
    () => {
        $crate::Len::<{ 1 + 2 }>.get()
    };
}

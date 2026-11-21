#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Symbols {
    pub file_do: usize,
    pub root_source: usize,
    pub root_destination: usize,
    pub root_op: usize,
    pub src_parent: usize,
    pub dst_parent: usize,
    pub property_info: usize,
    pub undo_to: usize,
    pub redo_to: usize,
}

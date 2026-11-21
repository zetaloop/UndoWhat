#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Symbols {
    pub file_do: usize,
    pub root_source: usize,
    pub root_destination: usize,
    pub root_op: usize,
    pub node_count: usize,
    pub node_child: usize,
    pub node_item: usize,
    pub undo_to: usize,
    pub redo_to: usize,
}

// Procedural shader node graph — modular layout (one file per node under nodes/)
pub mod types;
pub mod node_type;
pub mod ops;
pub mod node_data;
pub mod nodes;
pub mod graph;
pub mod palette;
pub mod presets;
pub mod editor;

pub use types::*;
pub use ops::*;
// NoiseBlendCurve re-exported via ops::*
pub use node_type::NodeType;
pub use node_data::{Node, NodeId};
pub use graph::{ChannelMeta, Connection, LiveGpuNoise, NodeGraph};
pub use palette::NodePalette;
pub use presets::MaterialPresets;
pub use editor::{
    draw_node_editor, AddNodeMenu, ColorPopupState, ColorPopupTarget, LinkDropMenu,
    NodeDragState, NodeEditorOutcome,
};

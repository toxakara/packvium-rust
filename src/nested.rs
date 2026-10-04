use crate::{PackResult, PackingRequest, PackingResult, pack_request};

#[derive(Clone, Debug)]
pub struct NestedLevel {
    pub name: String,
    pub request: PackingRequest,
}

#[derive(Clone, Debug)]
pub struct NestedPackingRequest {
    pub levels: Vec<NestedLevel>,
    pub beam_width: usize,
}

#[derive(Clone, Debug)]
pub struct NestedPackingResult {
    pub levels: Vec<(String, PackingResult)>,
}

/// Executes explicitly supplied levels in order.
///
/// A level whose request lists no items packs the previous level's containers instead, each
/// as one item (`PackedContainer::as_item`), as Python's `NestedPacker` does. Such a level is
/// not run after a level that left something unpacked: the chain ends there, since packing
/// part of an order onto a pallet would pass for the whole of it. A level that lists its own
/// items always runs on them, exactly as before chaining existed. A non-zero `beam_width`
/// sets every level's `container_plan_beam_width`.
pub fn pack_nested(request: &NestedPackingRequest) -> PackResult<NestedPackingResult> {
    let mut levels: Vec<(String, PackingResult)> = Vec::new();
    for level in &request.levels {
        let mut level_request = level.request.clone();
        if level_request.items.is_empty()
            && let Some((_, previous)) = levels.last()
        {
            if !previous.complete() {
                break;
            }
            level_request.items = previous.containers.iter().map(|c| c.as_item()).collect();
        }
        if request.beam_width > 0 {
            level_request.config.container_plan_beam_width = request.beam_width;
        }
        levels.push((level.name.clone(), pack_request(&level_request)?));
    }
    Ok(NestedPackingResult { levels })
}

import Foundation

/// Greedy interval partitioning: assigns each time-ranged item to the first
/// sub-row it fits in, so overlapping lane clips stack instead of colliding.
/// Items that would exceed `maxRows` overlap in the last row.
nonisolated func packLaneRows<Item>(
    _ items: [Item],
    maxRows: Int,
    start: (Item) -> TimeInterval,
    length: (Item) -> TimeInterval
) -> [(item: Item, row: Int)] {
    let sorted = items.sorted { start($0) < start($1) }
    var rowEnds: [TimeInterval] = []
    var placed: [(item: Item, row: Int)] = []

    for item in sorted {
        let itemStart = start(item)
        if let row = rowEnds.indices.first(where: { rowEnds[$0] <= itemStart + 0.001 }) {
            rowEnds[row] = itemStart + length(item)
            placed.append((item, row))
        } else if rowEnds.count < maxRows {
            rowEnds.append(itemStart + length(item))
            placed.append((item, rowEnds.count - 1))
        } else {
            rowEnds[rowEnds.count - 1] = itemStart + length(item)
            placed.append((item, rowEnds.count - 1))
        }
    }
    return placed
}

/// Number of sub-rows a packed lane occupies (0 when empty).
nonisolated func laneRowCount<Item>(
    _ items: [Item],
    maxRows: Int,
    start: (Item) -> TimeInterval,
    length: (Item) -> TimeInterval
) -> Int {
    packLaneRows(items, maxRows: maxRows, start: start, length: length)
        .map { $0.row + 1 }
        .max() ?? 0
}

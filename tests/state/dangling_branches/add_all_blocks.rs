use std::path::PathBuf;

use mina_indexer::{
    block::{parser::BlockParser, Block},
    state::State,
};

/// Parses all blocks in ./tests/data/beautified_sequential_blocks
/// Adds them to a fresh state
/// Verifies the faithfullness of the correspondence between dangling branch `leaves` and the underlying tree's leaf blocks
/// Verifies the length of the longest chain
#[tokio::test]
async fn extension() {
    let log_dir = PathBuf::from("./tests/data/beautified_sequential_blocks");
    let mut block_parser = BlockParser::new(&log_dir).unwrap();

    let mut n = 0;
    if let Some(precomputed_block) = block_parser.next().await.unwrap() {
        let mut state = State::new(&precomputed_block, None).unwrap();
        n += 1;
        while let Some(precomputed_block) = block_parser.next().await.unwrap() {
            state.add_block(&precomputed_block);
            n += 1;
        }

        // All 24 blocks are parsed successfully
        println!("Blocks added: {n}");
        assert_eq!(n, 24);

        // 3 dangling branch
        // - 0: height = 10
        // - 1: height = 1
        // - 2: height = 1
        // - 1 leaf
        assert_eq!(state.dangling_branches.clone().len(), 3);
        state
            .dangling_branches
            .iter()
            .enumerate()
            .for_each(|(idx, tree)| {
                if idx == 0 {
                    assert_eq!(tree.branches.height(), 10);
                } else {
                    assert_eq!(tree.branches.height(), 1);
                }
            });
        state
            .dangling_branches
            .iter()
            .enumerate()
            .for_each(|(idx, tree)| {
                if idx == 0 {
                    assert_eq!(tree.leaves.len(), 12);
                } else {
                    assert_eq!(tree.leaves.len(), 1);
                }
            });

        // dangling branches
        let branches0 = &state.dangling_branches.get(0).unwrap().branches;
        let branches1 = &state.dangling_branches.get(1).unwrap().branches;
        let branches2 = &state.dangling_branches.get(2).unwrap().branches;

        println!("=== Dangling Branch 0 ===");
        let mut tree = String::new();
        branches0.write_formatted(&mut tree).unwrap();
        println!("{tree}");

        println!("=== Dangling Branch 1 ===");
        let mut tree = String::new();
        branches1.write_formatted(&mut tree).unwrap();
        println!("{tree}");

        println!("=== Dangling Branch 2 ===");
        let mut tree = String::new();
        branches2.write_formatted(&mut tree).unwrap();
        println!("{tree}");

        // check all children.height = 1 + parent.height
        // check all children.parent_hash = parent.state_hash
        // check all children.parent_hash = parent.state_hash
        for (idx, dangling_branch) in state.dangling_branches.iter().enumerate() {
            for node_id in dangling_branch
                .branches
                .traverse_level_order_ids(dangling_branch.branches.root_node_id().unwrap())
                .unwrap()
            {
                let node = dangling_branch.branches.get(&node_id).unwrap();
                for child_id in node.children() {
                    let parent_block =
                        &dangling_branch.branches.get(&node_id).unwrap().data().block;
                    let child_block = &dangling_branch.branches.get(child_id).unwrap().data().block;
                    assert_eq!(child_block.height, 1 + parent_block.height);
                    assert_eq!(
                        child_block.blockchain_length.unwrap(),
                        1 + parent_block.blockchain_length.unwrap()
                    );
                    assert_eq!(
                        node.data().block.state_hash,
                        dangling_branch
                            .branches
                            .get(child_id)
                            .unwrap()
                            .data()
                            .block
                            .parent_hash
                    );
                }

                // Branch leaves faithfully correspond to the underlying tree's leaf blocks
                let leaves: Vec<&Block> = state
                    .dangling_branches
                    .get(idx)
                    .unwrap()
                    .leaves
                    .iter()
                    .map(|(_, node_block)| &node_block.block)
                    .collect();
                let tree_leaves: Vec<&Block> = state
                    .dangling_branches
                    .get(idx)
                    .unwrap()
                    .leaves
                    .iter()
                    .map(|(leaf_id, _)| {
                        // tree node corresponding to leaf_id
                        &state
                            .dangling_branches
                            .get(idx)
                            .unwrap()
                            .branches
                            .get(leaf_id)
                            .unwrap()
                            .data()
                            .block
                    })
                    .collect();
                assert_eq!(leaves, tree_leaves);
            }
        }

        // longest chain
        let longest_chain = state.dangling_branches.get(0).unwrap().longest_chain();
        let display_chain = longest_chain
            .iter()
            .map(|x| {
                (
                    x.block.height,
                    x.block.blockchain_length.unwrap_or(0),
                    &x.block.state_hash.0[0..12],
                )
            })
            .collect::<Vec<(u32, u32, &str)>>();
        println!("=== Longest dangling chain ===\n{display_chain:?}");

        // ten blocks in the longest chain
        assert_eq!(longest_chain.len(), 10);
    }
}
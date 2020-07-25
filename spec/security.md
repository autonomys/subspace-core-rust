## Security Requirements

Key Questions

1. Does the honest network converge in the presence of forks
2. Can an attacker gain an advantage through private simulation of different solutions?
3. Is there an incentive for nodes to gossip/solve ahead of the deadline (and simulate)?
4. What is the probability of winning by encoding on-demand?
5. How far back may a history rewriting attack be implemented?
6. Are there any incentives for selfish mining?
7. Is pooled mining even possible?
8. How ASIC resistant is Sloth?
9. Shortcuts in Sloth computation?
10. Do incentives work correclty for the plotting pieces closest to node_id?
11. Is there any advantage to employing the sybil attack (using multpile node ids)?
12. Does the g-greedy strategy allow for the balance attack?

### Honest Network Convergence

In a traditional proof-of-work blockchain, such as Bitcoin, it takes ten minutes, on average, for a miner to find a valid solution to the block challenge, while it takes less than thirty seconds for the new block to propogate to all miners on the network. In a proof-of-capacity blockchain, such as Subspace, it takes a few milliesconds for a farmer to solve the block challenge, but up to a second for the block to propagate across the network. 

Blocks take much less time to produce than to propagate across the network
Multiple blocks may be found that are above the quality threshold

For each challenge, there are as many potential solutions as there are replicas of the ledger, as each encoding of the piece under audit is valid. Critically, one encoding will always have the highest quality


For each block there is one challenge, with many possible solutions
How do we know which is the best solution

### Broadcasting Ahead of the Deadline

### Less than 50% Storage Attacks

### On-Demand Encoding (Space-Time Tradeoff)

### Simulation Attacks

### History Rewriting Attacks
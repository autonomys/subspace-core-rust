## Security Requirements

### Key Questions

1. How does the network recover from forks (accidental & intentional)?
2. Is there any advantage gained from intentionally forking the network?
3. How much storage power is needed for the private attack?
4. Is selfish farming even possible?
5. Do forking attacks allow for a balance attack?
6. How to eliminate public simulation?
7. How to constrain private simulation?
8. How far into the future can nodes solve (predict)?
9. How far into the future will nodes apply blocks?

* on-demand encoding (and hybrid version)
* Do we need incentives for uncles?
* Attacks on the shared clock
* Sybil attacks and piece/node-id proximity rule
* Are there incentives for pooled mining?
* history rewriting attacks
* ASIC and GPU resistance of Sloth (memory bandwidth hardness)
* discuss implications of multiple valid blocks per round (easier to fork/balance?)

### Goals

1. Explain how the honest network converges and manages forks
2. Show that less than 51% storage attacks are not possible with sufficient k-depth.
3. Show that substituting computation for storage is irrational.
4. Show that selfish farming is impossible.
5. Show that simulation (and history rewriting) are severely constrained.
6. Show how bribery attacks are constrained.

### Honest Network Convergence

* Operation of the honest network
* Defining a fork (two blocks at the same height with different parents)
* Probability of an accidental fork
* Intentionally creating forks (how & why)
* Recovery from a fork (fork choice rule)

### Private Attack

* How to execute and why
* Probability of success based on k-depth
* Caveats for Nakamoto's analysis
* Note on the balance attack (used ICW forking)
* Note on the sybil attack 

### Selfish Farming

* what is selfish mining
* define selfish farming
* is subspace race-free (like Spacemesh)
* show that no advantage may be gained

### Time-Space Trade-off Attacks

* show how much computation is needed for naive encoding on-demand
* describe the compression attack
* show how much computation is needed for the compression attack
* describe the countermeasure for the computation attack
* show how much computation is needed based on the nonce update interval
* show implications for fairness attacks
* show implication for 51% computation attacks
* examine combined storage and computation attacks

### Simulation Attacks

* state the advantage gained from simulation
* show the impact of challenge re-use
* show how to derive the challenge securely
* state the expected advantage an attacker may gain
* show this applies to recent and deep forks

### Bribery Attacks

* show how far into the future any farmer may predict (within and between epochs)
* explain how the bribery attack works
* show the relation between k-depth and c-correlation to achieve security

### Remaining Assumptions

1. Random Oracles
2. Digital Signatures
3. Modular Square Roots
4. Shared Clock
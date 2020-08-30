## Protocol Summary

Given only the genesis block hash and the expected block time:

### High Level Idea

* The confirmed archival state of the ledger (i.e. the blockchain history) is sliced into 4096 byte pieces.
* Pieces are erasure coded for redundancy under an m of n scheme
* Each consensus node (farmer) stores a unique copy the ledger
* Each piece is assymetrically time delay encoded using the Sloth algorithm, with the farmer_id as the key
* For each new block, all encodings from all plots are audited
* This is made efficient by storing commitments to plots in a binary search tree
* Each farmer searches for commitments that are below the quality threshold
* The threshold is tuned s.t. on average, there is one solution per challenge.
 
### Sync Phase (manual)

* Join the peer-to-peer (p2p) network through one or more bootstrap nodes.
* Populate peer routing table from bootstrap nodes.
* Connect to eight peers chosen an random
* Ask each peer for its current block height and block hash
* If peers are in agreement -> sync the ledger
* If peers do not agree -> sync multiple ledgers and compare
* Retrieve each block (starting from genesis)
* Validate and apply the block (minus the encoding)
* Retain blocks and tx in memory until enough state for an encoding has been obtained
* Erasure code the block and store pieces to disk
* After all confirmed blocks have been erasure coded they may be pruned

### Setup Phase

* Generate an ED25519-ECDSA public & private key pair from some source of randomness.
* Derive a node_id as hash(public_key).
* Retrieve 4kb pieces over the network
* Encode each piece with sloth, using node_id xor piece_index as the initilizaiton vector
* Store each encoding to disk (HDD or SSD)
* Compute a commitment (HMAC) for each encoding using a temorary nonce
* Store the commitment in a Binary Search Tree -- currently RocksDB on disk
* Retain a mapping of piece index to LBA in Rocks as well
* Continue until all available drive space is full, using as many different node ids as possible

### Challenge / Response Phase

* Challenge recycling
* Lookback parameter
* 
* For each new valid block
* Compute the challenge as hash(block.tag) -> opportunity for simulation (for each encoding)
* Compute the parent block for the piece audit as: challenge mod (block_height - encoding_delay_parameter)
* Where encoding_delay_parameter will point to last block that has been erasure coded by all nodes w.h.p.
* Determine the first state block that said block was erasure coded into
* Compute the piece_index as the 256 x state_block_index + (challenge mod 256)
* Each farmer reads the associated encoding for that piece from disk
* Compute the tag as hmac(encoding || challenge)
* If tag <= quality target -> forge a new block and gossip
* If tag > quality target, wait with exponential backoff based on distance, and gossip if no solution is yet seen

### Encoding State

* For each new valid block
* Canonicaly order the transactions
* Discard the encodings (we only need the tag)
* Fill a buffer with the binary block and tx data
* Continue for new blocks until the buffer reaches 512k
* Slice the buffer into 128 x 4096 byte pieces
* For each source piece, compute one parity piece
* We now have 256 x 4096 pieces ~ 1 MB
* Compute a merkle root over all 256 pieces
* Create a state block consisting of the merkle root and last state block hash ( < 100 bytes)
* Each farmer only has to retain the state chain to validate new solutions, if it include a merkle proof

### Efficient Sync
* First sync the state chain, verifying the hash chain
* Divide the state chain in half
* Choose a state block at random from the first half
* Verify the state block manually by obtaining 1/2 of piecees and erasure decoding
* Reconstruct one full block chosen at random and verify the encoding manually
* Repeat on the next half until the head of the chain is reach (log complexity)
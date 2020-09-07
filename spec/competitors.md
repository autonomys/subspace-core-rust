## Relationship to other Protocol

### BusrtCoin

[Burstcoin] (a.k.a. Burst), launched in 2014 by open-source developers, was the first blockchain protocol secured by spare disk capacity. Instead of using a proof-of-work, Burst uses a proof-of-capacity (PoC), in which users fill their free disk space with pseudo-random data seeded by their public key. The goal is to be more environmentally friendly and more ASIC resistant than protocols like Bitcoin, since computation is only required during the initial disk plotting phase. In 2015 the Burstcoin protocol was formally analyzed in the [Spacemint paper], which described a severe time-space trade-off attack on the PoC and a devastating history rewriting attack on the blockchain itself. The Burst team eventually implemented many of the design changes suggested by the authors of the Spacemint paper, at the cost of significantly increased proof sizes and requiring miners to pre-register on the blockchain before being allowed to propose new blocks. The second change results in a construction that is very different from Nakamoto’s Bitcoin, which allowed miners to come and go at will, and instead more closely resembles a proof-of-stake protocol. Despite these issues, Burst saw signifiant adoption amongst miners seeking to monetize spare disk resources and saw several hundred petabytes of space pledged at its height [link]. The fact that Burst also has long block times (roughly five minutes) has led to the creation of a handful of large mining pools [link], which leads to similar concerns over centralization as are present in the Bitcoin network. The Burst community was also rocked by a major scandal in 2019 when several of the core developers added malware into the reference client to merge mine and attack other protocols, keeping the funds for themselves [link]. Several Burst clones have also appeared including … that have slightly different approaches to the token economic policy but make no changes to the underlying blockchain technology. While the price of Burst has dropped over one-hundred fold from it highs during the 2018 bull market [link], it remains the only storage based blockchain protocol that has yet been launched in production no one has yet mounted a successful double spend attack on the network.

### Filecoin

Filecoin, originally proposed in 2014 by Juan Benet, is a blockchain protocol secured by proofs-of-storage of user-generated data. This can be understood as a proof-of-useful-space, in contrast to a proof-of-useless-space as employed within Burst, Chia, and Spacemesh. At a high level, users submit files to the network by referencing them in a blockchain transaction. Miners will then store these files and must answer periodic audits to be eligible to propose the next block. Critically, the files are not stored on-chain, only the transactions and proofs-of-storage. The proofs themselves have evolved significantly over the life of the project and currently include proofs-of-replication (PoR) and proofs-of-space-time (PoST), which are both implemented within a zero-knowledge (ZK) proof system. Generating these proofs is computationally expensive and the protocol assumes access to a GPU and high-end computer for both the initial plotting phase and the continuous challenge-response period.  It is unclear if these proofs are ASIC resistant, though several Chinese firms have already begun selling Filecoin mining rigs. Filecoin also differs from Nakamoto consensus in that it is purposefully designed to be a permissioned network. In a variant inspired by proof-of-stake, Miners must pre-register on-chain in order to be eligible to store files on behalf of users. The need for high-end hardware combined with the the permissioned nature of the protocol raise concerns over how decentralized the network will actually be. On the current test-net six mining pools (all based in China) control over half of the network. Despite raising several hundred million dollars in a 2017 ICO, Filecoin remains a work in progress, six years after it was initially proposed. While a test-net is live, a main-net launch date has yet to be announced and the protocol still has unresolved security issues. This is not surprising given its scope and complexity, as described in the most recent white paper. Generally speaking, a simpler protocol presents fewer security assumptions and a smaller attack surface area as well as a smaller code base and a smaller potential for bugs. In many ways the Filecoin project reflects the problems inherent in the opposite approach. Furthermore, the computational complexity and associated energy costs of mining PoRs and PoST in a ZK system raise serious questions about the suitability of storing real world data on the network, it may simply be too expensive for most use cases. 

### Chia Network

Chia is a much simpler blockchain based on proofs of space and time that seeks to maintain the decentralized and permissionless nature of Bitcoin while greatly reducing the energy costs of consensus. Originally proposed in 2017 by Bram Cohen, creator of the BitTorrent network, Chia currently has a working test-net and plans to go live later this year. Chia employs a novel extension of Nakamoto consensus which alternates proofs of space and proofs of time to produce a mining dynamic that mimics the Bitcoin network. For each new block, space farmers compete to provide the best proof-of-space, the quality of which determines the delay parameter for a proof-of-time. Time lords finalize the block with a proof-of-time, which is non-parallelizable but still takes an average of five minutes to compute with high-end hardware. While it only takes a single fast and honest time lord to keep the network running, this still adds a new assumption to the security model and provides a weaker definition of decentralization than the original Bitcoin vision. Furthermore, a yet to be designed proof-of-time ASIC may lead to further centralization of control and allow for double-spends or selfish farming with less than a majority share of the storage resources. In fact, in its current configuration, Chia requires a super-majority (61.5%) of space farmers to be honest to prevent double-spend attacks, compared to the greater than 50% majority assumption of Nakamoto consensus under a proof-of-work regime. Chia retains the low transaction throughput and high confirmation times of Bitcoin, and plans to rely on layer two network such as lightning for scaling, despite many recent advances in layer one techniques. This perhaps makes sense given that they do not seek to be a viable form of digital cash for everyday use, but rather a better form of digital gold than Bitcoin for national and corporate backed colored coins.

### Spacemint Proposal

### Spacemesh

Rely on a different proof of space time
Different proof of space
Proof of Elapsed Time
Uses the tortoise and hare protocol, a complex BA agreement process
2/3 honest majority assumption
High tx throughput but high confirmation latency
Permissioned, requires registration before each round of farming (merkle roots)


### Compare & Contrast

It is difficult to compare proof-of-capacity protocols in an apples to apples fashion, as each protocol was designed for a specific purpose and makes different trade-offs to achieve it. This analysis is made more difficult by the fact that protocol design is a dynamic and iterative process. Many of the protocols in questions are still changing, some on a daily basis, and many of the published reference materials are already outdated.

Here we attempt adopt a simle benchmark for comparison, how does it measure against the original goals of Bitcoin as a means of digital cash. Ideally it would maintain the 1/2 security bound, the fairness of one-disk-one-vote, and allow for high throughput and low latency transactions.

First we note that all proof-of-capacity protocols begin by replacing proof-of-work with a proof-of-storage/space/space-time/replication. These may be broadly categorized into proofs-of-storage, which are based on some real-world data, and proofs-of-space, which are based on some randomly generated data. Subspace and Filecoin follow the former, while Burst, Spacemint, Chia, and Spacemesh follow the latter. The distinction is important as proofs-of-useful storage are less susceptible 

The farmer's dilemma    

Useful Storage: Only Filecoin, but a much more limited notion of what may be added

Permissionless: Only Chia

Security: Perhaps only Burst, maybe Filecoin

Scalability: Spacemesh comes close to tx throughput

Decentralization: Chia requires time lords 

Usability -- Hardware Support

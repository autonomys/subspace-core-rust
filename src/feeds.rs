type PostId = [u8; 32];
type PublicKey = [u8; 32];

struct Post {
    /// The feed which is being appended
    address: PublicKey,
    /// The last post in this feed
    parent: PostId,
    /// The auto-incrementing sequence number for this post
    sequence: u64,
    /// The time when this post was created
    timestamp: u64,
    /// The post content
    content: Content,
    /// Signature of the post with corresponding private key
    signature: [u8; 32],
}

// TODO: handle encrypted data, supporting decryption with multiple public keys

struct Content {
    /// optional application ID
    app_id: [u8; 32],
    /// optional symmetric encryption key, assymetrically encrypted with Post Private Key
    key: [u8; 32],
    /// data: optionally encrypted
    data: Vec<u8>,
}

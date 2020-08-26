# Laconic

A scripting language with few words.
Human readable
Composition by chaining

## Examples

### Coinbase Tx


```typescript
    mint(1)
        .to(Alice)
        .send()

```

### Simple Tx

Send 1 credit from from Alice to Bob

```typescript
    Alice
        .take(1)
        .to(Bob)
        .send()

    Alice
        .take(1)
        .to(Carol)
        .send()
```
### Multi-Sig Tx

Send 1 credits from Alice to Bob and Charlie
Bob and Charlie must both sign to spend.

```typescript
    Alice
        .take(1)
        .to([Bob, Charlie])
        .send()
```

Send 1 credits from Alice to Bob and Charlie
Either Bob or Charlie may sign to spend.



```typescript

    let recipients = [Bob, Charlie];
    let multissig_account = account_with_approval(recpients, 2);

    Alice
        .take(1)
        .to(Bob)
        .to(Charlie)
        .with_approval_from(2)
        .include("my_script.sol")
        .send()
```

### Time-Lock Tx

Send 1 Credits from Alice to Bob.

```typescript
    Alice
        .take(1)
        .to(Bob)
        .after_time("2020-Sep-21 18:37:21")
        .send()
```

```typescript
    Alice
        .take(1)
        .to(Bob)
        .after_block(345_772)
        .send()
```

### Atmoic Swap
    
Swap 10,000 Credits for one BTC

1. Alice creates an ask offer
2. Bob reply with a bid offer (that signs)
3. Alice and Bob sign the offer
4. Alice commits to Subspace
5. Bob commits to Bitcoin

```typescript
    send(10_000)
        .from(Alice)
        .to(Bob)
        .in_exchange_for(1, BTC)
```

```typescript
    send(10_000)
        .from(Alice)
        .in_exchange_for(1, BTC)
        .first()
```

### State Channel

```typescript
    send(10_000)
        .from(Alice)
        .into_channel_with(Bob)
        .expiring_in(1, hour)
```

### Storage

```typescript
    

```

### Timestamp

### Create a Token

```typescript
    issue()

```

### Create a 
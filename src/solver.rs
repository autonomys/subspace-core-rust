#![allow(dead_code)]

/* ToDo
 * 
 * Given a random seed
 * Hash the seed to determine the audit target
 * Modulo the audit target by the piece count (or block height)
 * Read the associated encoding from disk
 * Compute an hmac over the encoding using the challenge
 * Measure the quality of the solution
 * Return as a solution struct
 * Build as an eval loop with async channel
 * Add in a delay parameter
 * Hanlde exceptions
*/
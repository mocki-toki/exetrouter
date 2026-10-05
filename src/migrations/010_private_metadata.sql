CREATE TABLE private_metadata (
 user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
 object_id TEXT NOT NULL CHECK(length(object_id)=64),
 ciphertext TEXT NOT NULL CHECK(length(ciphertext)<=16384),
 PRIMARY KEY(user_id,object_id)
);

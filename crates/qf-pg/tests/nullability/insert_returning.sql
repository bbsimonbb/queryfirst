-- expect: id=notnull name=nullable:expression created_at=notnull
INSERT INTO customers (email) VALUES ($1) RETURNING id, name, created_at

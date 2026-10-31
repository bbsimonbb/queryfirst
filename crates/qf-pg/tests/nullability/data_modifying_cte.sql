-- expect: id=notnull name=nullable:expression
WITH ins AS (INSERT INTO customers (email) VALUES ($1) RETURNING id, name)
SELECT ins.id, ins.name FROM ins

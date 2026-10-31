-- expect: email=notnull e2=nullable:outer-join
SELECT c.email, d.email AS e2 FROM customers c LEFT JOIN customers d ON d.id = c.id

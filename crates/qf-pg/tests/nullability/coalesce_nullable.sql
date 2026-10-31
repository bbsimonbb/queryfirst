-- expect: x=nullable:expression y=nullable:outer-join
SELECT coalesce(c.name, c.birthday::text) AS x, coalesce(a.line1, a.city) AS y
FROM customers c LEFT JOIN addresses a ON a.customer_id = c.id

-- expect: "name!"=notnull:override "id?"=nullable:override "line1!"=notnull:override
SELECT c.name AS "name!", c.id AS "id?", a.line1 AS "line1!"
FROM customers c LEFT JOIN addresses a ON a.customer_id = c.id

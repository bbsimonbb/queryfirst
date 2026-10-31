-- expect: email=nullable:expression
WITH customers AS (SELECT name AS email FROM customers)
SELECT email FROM customers

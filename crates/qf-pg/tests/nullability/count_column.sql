-- expect: count=notnull distinct_cities=notnull
SELECT count(a.city), count(DISTINCT a.city) AS distinct_cities
FROM customers c LEFT JOIN addresses a ON a.customer_id = c.id

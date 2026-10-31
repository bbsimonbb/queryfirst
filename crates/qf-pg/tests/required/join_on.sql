-- expect: addressId=required:where-eq-notnull
SELECT c.id FROM customers c JOIN addresses a ON a.customer_id = c.id AND a.id = :addressId

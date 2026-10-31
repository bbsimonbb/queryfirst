-- expect: addressId=nullable
SELECT id FROM customers WHERE id = (SELECT customer_id FROM addresses WHERE id = :addressId)

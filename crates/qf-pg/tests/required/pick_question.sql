-- expect: address.customerId=nullable:annotation address.line1=nullable:annotation address.city=nullable:annotation
/*
 @name insertAddress
 @param address -> (customerId, line1, city)
*/
INSERT INTO addresses (customer_id, line1, city) VALUES :address?

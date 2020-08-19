CREATE USER IF NOT EXISTS
    hawkbit
IDENTIFIED BY
    'hawkbit'
;

GRANT ALL ON hawkbit.* to 'hawkbit';

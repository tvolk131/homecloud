CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS users(id TEXT PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL CHECK(role IN ('admin','user')), active INTEGER NOT NULL, grant_json TEXT NOT NULL, gpu_allowed INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS devices(peer TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id), active INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS invitations(verifier TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id), expires_at INTEGER NOT NULL, consumed_at INTEGER);
CREATE TABLE IF NOT EXISTS vms(id TEXT PRIMARY KEY, owner TEXT NOT NULL REFERENCES users(id), record TEXT NOT NULL);
-- Leases precede VM creation in a single transaction, so their VM foreign key
-- is deferred until commit. Failed transactions cannot strand a reservation.
CREATE TABLE IF NOT EXISTS cpu_leases(cpu_id INTEGER PRIMARY KEY, vm_id TEXT NOT NULL REFERENCES vms(id) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE IF NOT EXISTS network_leases(slot INTEGER PRIMARY KEY, vm_id TEXT NOT NULL UNIQUE REFERENCES vms(id) DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE IF NOT EXISTS gpu_leases(pci TEXT PRIMARY KEY, vm_id TEXT NOT NULL REFERENCES vms(id) DEFERRABLE INITIALLY DEFERRED);
-- PCI addresses identify the same device regardless of hex letter case. This
-- also checks existing databases; conflicting historical claims fail closed.
CREATE UNIQUE INDEX IF NOT EXISTS gpu_identity ON gpu_leases(pci COLLATE NOCASE);
CREATE TABLE IF NOT EXISTS operations(id TEXT PRIMARY KEY, actor TEXT NOT NULL REFERENCES users(id), request_key TEXT NOT NULL, vm_id TEXT NOT NULL REFERENCES vms(id), phase TEXT NOT NULL, record TEXT NOT NULL, UNIQUE(actor,request_key));
CREATE UNIQUE INDEX IF NOT EXISTS one_active_operation_per_vm ON operations(vm_id) WHERE phase NOT IN ('succeeded','failed');
CREATE TABLE IF NOT EXISTS audit(id INTEGER PRIMARY KEY, timestamp INTEGER NOT NULL, actor TEXT NOT NULL, action TEXT NOT NULL, target TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS applications(id TEXT PRIMARY KEY, vm_id TEXT NOT NULL REFERENCES vms(id), kind TEXT NOT NULL, record TEXT NOT NULL, UNIQUE(vm_id,kind));
CREATE TABLE IF NOT EXISTS application_operations(id TEXT PRIMARY KEY, application_id TEXT NOT NULL REFERENCES applications(id), actor TEXT NOT NULL REFERENCES users(id), request_key TEXT NOT NULL, phase TEXT NOT NULL, record TEXT NOT NULL, UNIQUE(actor,request_key));
CREATE UNIQUE INDEX IF NOT EXISTS one_active_application_operation ON application_operations(application_id) WHERE phase IN ('pending','running','waiting_for_user');

CREATE TABLE IF NOT EXISTS host_reserve_operations(id TEXT PRIMARY KEY, actor TEXT NOT NULL REFERENCES users(id), request_key TEXT NOT NULL, phase TEXT NOT NULL, record TEXT NOT NULL, UNIQUE(actor,request_key));
CREATE UNIQUE INDEX IF NOT EXISTS one_active_reserve_operation ON host_reserve_operations((1)) WHERE phase NOT IN ('succeeded','cancelled');

-- Rejected recoverable Creates fence delayed copies of the same request key.
CREATE TABLE IF NOT EXISTS create_rejections(actor TEXT NOT NULL REFERENCES users(id), request_key TEXT NOT NULL, request_json TEXT NOT NULL, message TEXT NOT NULL, PRIMARY KEY(actor,request_key));

CREATE TABLE IF NOT EXISTS memory_capacity_operations(id TEXT PRIMARY KEY, actor TEXT NOT NULL REFERENCES users(id), terminal INTEGER NOT NULL, record TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS one_pending_memory_capacity ON memory_capacity_operations((1)) WHERE terminal=0;

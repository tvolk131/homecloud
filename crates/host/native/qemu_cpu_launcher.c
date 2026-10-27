#define _GNU_SOURCE
/* Lab prototype: move QEMU into a pre-created per-VM cpuset before exec.
 * No shell, arbitrary commands, guest input, or user-writable policy.
 */
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static void fail(const char *s) { perror(s); exit(125); }
static void cpus(const char *s, cpu_set_t *set) {
    char copy[1024];
    if (strlen(s)>=sizeof(copy)) { errno=EINVAL; fail("CPU list length"); }
    strcpy(copy,s); CPU_ZERO(set);
    for (char *p=strtok(copy,",");p;p=strtok(NULL,",")) {
        char tail;int a,b;
        if(sscanf(p,"%d-%d%c",&a,&b,&tail)==2) {}
        else if(sscanf(p,"%d%c",&a,&tail)==1) b=a;
        else { errno=EINVAL; fail("CPU list syntax"); }
        if(a<0||b<a||b>=CPU_SETSIZE) { errno=EINVAL; fail("CPU range"); }
        for(int i=a;i<=b;i++) CPU_SET(i,set);
    }
    if(CPU_COUNT(set)==0) { errno=EINVAL; fail("Empty CPU set"); }
}
int main(int argc,char **argv) {
    char name[256]={0};int is_vm=0;
    for(int i=1;i<argc;i++) {
        if(!strcmp(argv[i],"-S"))is_vm=1;
        if(!strcmp(argv[i],"-name")&&i+1<argc) {
            if(strlen(argv[i+1])>=sizeof(name)) { errno=EINVAL;fail("VM name"); }
            strcpy(name,argv[i+1]);char *comma=strchr(name,',');if(comma)*comma=0;
        }
    }
    /* Incus's non-running QEMU feature probes have no instance name. */
    if(is_vm&&name[0]) {
        int fd=open("/etc/homecloud/cpu-policy.tsv",O_RDONLY|O_NOFOLLOW|O_CLOEXEC);
        if(fd<0)fail("CPU policy unavailable");
        struct stat st;if(fstat(fd,&st)||st.st_uid!=0||(st.st_mode&022)||!S_ISREG(st.st_mode)) {
            errno=EPERM;fail("CPU policy ownership");
        }
        FILE *f=fdopen(fd,"r");if(!f)fail("CPU policy read");
        char line[4096],vm[256],allowed[1024],house[1024],group[512];int found=0;
        while(fgets(line,sizeof(line),f)) {
            if(line[0]=='#')continue;
            if(sscanf(line,"%255s %1023s %1023s %511s",vm,allowed,house,group)==4&&!strcmp(vm,name)) {found=1;break;}
        }
        fclose(f);
        if(!found) { errno=EPERM;fail("No CPU allocation for named VM"); }
        if(strncmp(group,"/sys/fs/cgroup/homecloud.slice/homecloud-",sizeof("/sys/fs/cgroup/homecloud.slice/homecloud-")-1)||strstr(group,"..")) {
            errno=EPERM;fail("Invalid VM cgroup");
        }
        char path[1024];snprintf(path,sizeof(path),"%s/cgroup.procs",group);
        fd=open(path,O_WRONLY|O_NOFOLLOW|O_CLOEXEC);if(fd<0)fail("VM cpuset unavailable");
        char pid[32];int n=snprintf(pid,sizeof(pid),"%d",getpid());
        if(write(fd,pid,n)!=n)fail("Move to VM cpuset");
        close(fd);
        cpu_set_t expected,effective,house_set;cpus(allowed,&expected);cpus(house,&house_set);
        snprintf(path,sizeof(path),"%s/cpuset.cpus.effective",group);
        fd=open(path,O_RDONLY|O_NOFOLLOW|O_CLOEXEC);if(fd<0)fail("Read effective cpuset");
        n=read(fd,line,sizeof(line)-1);if(n<=0)fail("Empty effective cpuset");close(fd);
        line[n]=0;line[strcspn(line,"\n")]=0;cpus(line,&effective);
        if(!CPU_EQUAL(&expected,&effective)) { errno=EPERM;fail("Unexpected effective cpuset"); }
        for(int i=0;i<CPU_SETSIZE;i++)if(CPU_ISSET(i,&house_set)&&!CPU_ISSET(i,&effective)) {
            errno=EPERM;fail("Housekeeping outside VM cpuset");
        }
        if(sched_setaffinity(0,sizeof(house_set),&house_set))fail("Initial QEMU helper affinity");
        fd=open("/var/log/homecloud-cpu-launch.jsonl",O_WRONLY|O_APPEND|O_CLOEXEC|O_NOFOLLOW);
        if(fd<0)fail("Launch audit unavailable");
        dprintf(fd,"{\"vm\":\"%s\",\"launcher_pid\":%d,\"cpuset\":\"%s\",\"initial_affinity\":\"%s\",\"before_qemu_exec\":true}\n",name,getpid(),allowed,house);
        close(fd);
    }
    argv[0]="/usr/bin/qemu-system-x86_64";
    execv(argv[0],argv);fail("QEMU exec");
    return 125;
}

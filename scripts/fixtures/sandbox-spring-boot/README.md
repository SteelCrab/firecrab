# Sandbox Debug → Test fixture

This offline QA project deliberately fails application startup because
`GreetingController` requires a `String` bean that the application does not define.
It is not a production application or a dependency recommendation.

Provision Java 17 or newer, Maven, and a complete dependency cache outside the
project copy before disabling network access. Use bounded commands:

```sh
mvn -o -q -Dmaven.repo.local=/opt/firecrab/qa/m2 test
```

The initial run must fail with a missing `java.lang.String` bean. The known fix is
to add an `org.springframework.context.annotation.Bean` method to `Application`:

```java
@org.springframework.context.annotation.Bean
String greeting() {
    return "ok";
}
```

Debug must reproduce the failure, add the fix to its isolated copy, then run the
same regression test successfully. Test must wait for Debug's successful durable
result, receive its reviewed changes, and run the regression independently.
Record command event IDs, actual stdout/stderr and exit codes, test provenance,
artifacts, and unchanged original source. A model's assertion alone is not proof.

Spring Boot is pinned to 3.5.0 for this reproducible fixture. The Java and Maven
requirements follow the [official Spring Boot 3.5 requirements](https://docs.spring.io/spring-boot/3.5/system-requirements.html).

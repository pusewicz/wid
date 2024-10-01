# frozen_string_literal: true

module Wid
  class CodegenTest < Test
    def test_generate
      output = Wid.generate("1 + 2.0")

      assert_equal(<<~C, output)
        #include <Wid.h>

        int main() {
          (1 + 2.0);

          return 0;
        }
      C
    end

    def test_generate_nil
      output = Wid.generate("nil")

      assert_equal(<<~C, output)
        #include <Wid.h>

        int main() {
          nil;

          return 0;
        }
      C
    end

    def test_var_assignment
      output = Wid.generate("foo = 1")

      assert_equal(<<~C, output)
        #include <Wid.h>

        int main() {
          int foo = 1;

          return 0;
        }
      C
    end

    def test_var_assignment_string
      output = Wid.generate('foo = "bar"')

      assert_equal(<<~C, output)
        #include <Wid.h>

        int main() {
          char *foo = "bar";

          return 0;
        }
      C
    end

    def test_function_call
      output = Wid.generate('puts("Hello, world!")')

      assert_equal(<<~C, output)
        #include <Wid.h>
        #include <stdio.h>

        int main() {
          puts("Hello, world!");

          return 0;
        }
      C
    end

    def test_function_call_with_variable_passed
      output = Wid.generate('puts(foo)', debug: true)

      assert_equal(<<~C, output)
        #include <Wid.h>
        #include <stdio.h>

        int main() {
          puts(foo);

          return 0;
        }
      C
    end

    def test_binary_operation
      output = Wid.generate(<<~WID)
        number = 1 + 2.0
        print("Number %s", number)
      WID

      assert_equal(<<~C, output)
        #include <Wid.h>

        int main() {
          (1 + 2);

          return 0;
        }
      C
    end
  end
end

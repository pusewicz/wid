require "bigdecimal"

module Wid
  class TypeCheckerNumericTest < Test
    def test_integer
      assert_type(Wid::Type::Int, 1)
      assert_type(Wid::Type::Int, 24)
      assert_type(Wid::Type::Int, 3)
      assert_type(Wid::Type::Int, -3)
    end

    def test_float
      assert_type(Wid::Type::Float, 1.0)
      assert_type(Wid::Type::Float, 2.578)
      assert_type(Wid::Type::Float, 3.0)
      assert_type(Wid::Type::Float, -3.2)
    end

    def test_double
      assert_type(Wid::Type::Double, BigDecimal("1.324234"))
      assert_type(Wid::Type::Double, BigDecimal("-1.123123"))
    end

    def test_string
      assert_type(Wid::Type::String, '"foo"')
      assert_type(Wid::Type::String, '"bar"')
    end

    private

    def assert_type(expected, input)
      actual = TypeChecker.typecheck(input)
      assert_equal(expected, actual, "Expected #{expected}, but got #{actual} for `#{input}'")
    end
  end
end

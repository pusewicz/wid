module Wid
  module Nodes
    class FunctionCall < Expression
      attr_accessor :name, :arguments

      def initialize(name = nil, arguments = [])
        @arguments = arguments || []
        super(name)
      end

      alias_method :name, :value

      def function_name
        @value.name
      end

      def ==(other)
        children == other&.children
      end

      def children
        [@value, @arguments]
      end
    end
  end
end
